//! Slice-5 scoped Bayesian contextual ranking with finite deployed-policy
//! propensities.
//!
//! The policy is deliberately narrow: it can reorder only already eligible,
//! already routed representatives inside the configured first-page pool. It
//! never creates candidates, changes lanes, or invokes an external action.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    AttentionDecisionContext, AttentionDecisionItem, AttentionLabelQuality, AttentionOutcomeKind,
    AttentionSurface, ACTIONABILITY_FEATURE_CONTRACT, ATTENTION_PAIR_FEATURE_CONTRACT,
    ATTENTION_ROUTING_FEATURE_CONTRACT, ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
    ATTENTION_SEMANTIC_SCHEMA_VERSION,
};
use crate::config::AttentionBanditMode;

pub const ATTENTION_BANDIT_SCHEMA_VERSION: u32 = 1;
pub const ATTENTION_BANDIT_FEATURE_CONTRACT: &str = "attention_bandit_features_v1";
pub const ATTENTION_BANDIT_SEED_CONTRACT: &str = "blake3-counter-box-muller-v1";
pub const MAX_BANDIT_FEATURES: usize = 64;
pub const MAX_BANDIT_POSTERIOR_DRAWS: u32 = 16_384;
pub const MAX_BANDIT_FIRST_PAGE_SIZE: usize = 200;
pub const MIN_BANDIT_OBSERVATION_NOISE_VARIANCE: f64 = 1e-6;
pub const MAX_BANDIT_OBSERVATION_NOISE_VARIANCE: f64 = 1e6;
pub const MAX_BANDIT_REWARD_MAGNITUDE: f64 = 1.0;
pub const MAX_BANDIT_UPDATE_STRENGTH: f64 = 100.0;
pub const MAX_BANDIT_ATTRIBUTION_WINDOW_MS: i64 = 180 * 86_400_000;

const SUPPORTED_FEATURES: &[&str] = &[
    "routing.owner_action_required_probability",
    "routing.information_value_probability",
    "routing.follow_up_utility",
    "routing.worth_a_look_utility",
    "routing.uncertainty",
    "group.duplicate_exposure_cost_log1p",
    "context.queue_size_log1p",
    "context.recent_impression_count_log1p",
    "context.recent_action_count_log1p",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AttentionBanditTrainingManifest {
    pub dataset_digest: String,
    pub data_cutoff_at: i64,
    pub split_strategy: String,
    pub group_keys: Vec<String>,
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AttentionBanditRewardSpec {
    pub outcome: AttentionOutcomeKind,
    /// `None` is censored for this task and can never update the posterior.
    pub reward: Option<f64>,
    pub strong_strength: f64,
    pub weak_strength: f64,
    pub unknown_strength: f64,
}

impl AttentionBanditRewardSpec {
    pub fn strength(&self, quality: AttentionLabelQuality) -> f64 {
        match quality {
            AttentionLabelQuality::Strong => self.strong_strength,
            AttentionLabelQuality::Weak => self.weak_strength,
            AttentionLabelQuality::Unknown => self.unknown_strength,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AttentionBanditPolicySnapshot {
    pub snapshot_id: String,
    pub model_version: String,
    pub feature_contract: String,
    pub routing_feature_contract: String,
    pub routing_snapshot_id: String,
    pub routing_model_version: String,
    pub actionability_feature_contract: String,
    pub actionability_snapshot_id: String,
    pub actionability_model_version: String,
    pub grouping_feature_contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_model_version: Option<String>,
    pub semantic_schema_version: u32,
    pub semantic_extractor_contract: String,
    pub semantic_prompt_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_profile: Option<String>,
    /// Exact, ordered deployed feature vector. No serving weight exists
    /// outside this snapshot.
    pub feature_names: Vec<String>,
    pub prior_mean: Vec<f64>,
    pub prior_precision: Vec<Vec<f64>>,
    pub observation_noise_variance: f64,
    pub posterior_draw_count: u32,
    /// Mixture mass assigned to the uniform supported-action distribution.
    pub exploration_floor: f64,
    pub first_page_size: usize,
    pub slate_size: usize,
    pub seed_contract: String,
    pub seed_identity: String,
    pub reward_mapping: Vec<AttentionBanditRewardSpec>,
    pub attribution_window_ms: i64,
    pub require_verified_impression: bool,
    pub trained_at: i64,
    pub training_manifest: AttentionBanditTrainingManifest,
}

impl AttentionBanditPolicySnapshot {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.snapshot_id.trim().is_empty(),
            "bandit snapshot id is empty"
        );
        anyhow::ensure!(
            !self.model_version.trim().is_empty(),
            "bandit model version is empty"
        );
        anyhow::ensure!(
            self.feature_contract == ATTENTION_BANDIT_FEATURE_CONTRACT,
            "bandit feature contract is incompatible"
        );
        anyhow::ensure!(
            self.routing_feature_contract == ATTENTION_ROUTING_FEATURE_CONTRACT
                && !self.routing_snapshot_id.trim().is_empty()
                && !self.routing_model_version.trim().is_empty(),
            "bandit snapshot must pin an exact routing policy"
        );
        anyhow::ensure!(
            self.actionability_feature_contract == ACTIONABILITY_FEATURE_CONTRACT
                && !self.actionability_snapshot_id.trim().is_empty()
                && !self.actionability_model_version.trim().is_empty(),
            "bandit snapshot must pin an exact actionability policy"
        );
        anyhow::ensure!(
            self.grouping_feature_contract == ATTENTION_PAIR_FEATURE_CONTRACT,
            "bandit grouping feature contract is incompatible"
        );
        anyhow::ensure!(
            self.grouping_snapshot_id.is_some() == self.grouping_model_version.is_some(),
            "bandit grouping snapshot id and model version must both be present or absent"
        );
        anyhow::ensure!(
            self.grouping_snapshot_id
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty())
                && self
                    .grouping_model_version
                    .as_deref()
                    .is_none_or(|value| !value.trim().is_empty()),
            "bandit grouping identity is empty"
        );
        anyhow::ensure!(
            self.semantic_schema_version == ATTENTION_SEMANTIC_SCHEMA_VERSION
                && self.semantic_extractor_contract == ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT
                && !self.semantic_prompt_version.trim().is_empty(),
            "bandit semantic contract is incompatible"
        );
        anyhow::ensure!(
            self.semantic_model
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty())
                && self
                    .semantic_profile
                    .as_deref()
                    .is_none_or(|value| !value.trim().is_empty()),
            "bandit semantic producer identity is empty"
        );
        anyhow::ensure!(
            !self.feature_names.is_empty() && self.feature_names.len() <= MAX_BANDIT_FEATURES,
            "bandit feature count is outside the supported bound"
        );
        let mut unique = HashSet::new();
        for feature in &self.feature_names {
            anyhow::ensure!(
                unique.insert(feature),
                "duplicate bandit feature: {feature}"
            );
            anyhow::ensure!(
                SUPPORTED_FEATURES.contains(&feature.as_str()),
                "unsupported bandit feature: {feature}"
            );
        }
        anyhow::ensure!(
            self.prior_mean.len() == self.feature_names.len(),
            "bandit prior mean dimension mismatch"
        );
        validate_square_matrix(
            &self.prior_precision,
            self.feature_names.len(),
            "prior precision",
        )?;
        cholesky(&self.prior_precision)
            .map_err(|_| anyhow::anyhow!("bandit prior precision must be positive definite"))?;
        anyhow::ensure!(
            self.prior_mean.iter().all(|value| value.is_finite())
                && self.observation_noise_variance.is_finite()
                && (MIN_BANDIT_OBSERVATION_NOISE_VARIANCE..=MAX_BANDIT_OBSERVATION_NOISE_VARIANCE)
                    .contains(&self.observation_noise_variance),
            "bandit observation noise variance is outside the numerical-stability bound"
        );
        anyhow::ensure!(
            (1..=MAX_BANDIT_POSTERIOR_DRAWS).contains(&self.posterior_draw_count),
            "bandit posterior draw count is outside the supported bound"
        );
        anyhow::ensure!(
            self.exploration_floor.is_finite()
                && self.exploration_floor > 0.0
                && self.exploration_floor <= 1.0,
            "bandit exploration floor must be within (0, 1] so every supported action has positive logging support"
        );
        anyhow::ensure!(
            (1..=MAX_BANDIT_FIRST_PAGE_SIZE).contains(&self.first_page_size)
                && (1..=self.first_page_size).contains(&self.slate_size),
            "bandit first-page/slate bounds are invalid"
        );
        anyhow::ensure!(
            self.seed_contract == ATTENTION_BANDIT_SEED_CONTRACT
                && !self.seed_identity.trim().is_empty(),
            "bandit seed contract is incompatible"
        );
        let all_outcomes = [
            AttentionOutcomeKind::Useful,
            AttentionOutcomeKind::ActionCompleted,
            AttentionOutcomeKind::Irrelevant,
            AttentionOutcomeKind::NotActionable,
            AttentionOutcomeKind::Obsolete,
            AttentionOutcomeKind::NotOwner,
            AttentionOutcomeKind::DuplicateOf,
            AttentionOutcomeKind::NeutralSeen,
            AttentionOutcomeKind::TimingNegative,
        ];
        anyhow::ensure!(
            self.reward_mapping.len() == all_outcomes.len(),
            "bandit reward mapping must explicitly cover every canonical outcome"
        );
        for outcome in all_outcomes {
            let matching: Vec<_> = self
                .reward_mapping
                .iter()
                .filter(|mapping| mapping.outcome == outcome)
                .collect();
            anyhow::ensure!(
                matching.len() == 1,
                "bandit reward mapping is not one-to-one"
            );
            let mapping = matching[0];
            anyhow::ensure!(
                mapping.reward.is_none_or(|reward| {
                    reward.is_finite() && reward.abs() <= MAX_BANDIT_REWARD_MAGNITUDE
                }) && [
                    mapping.strong_strength,
                    mapping.weak_strength,
                    mapping.unknown_strength,
                ]
                .into_iter()
                .all(|strength| {
                    strength.is_finite() && (0.0..=MAX_BANDIT_UPDATE_STRENGTH).contains(&strength)
                }),
                "bandit reward/strength mapping is invalid"
            );
            if matches!(
                outcome,
                AttentionOutcomeKind::NeutralSeen
                    | AttentionOutcomeKind::DuplicateOf
                    | AttentionOutcomeKind::TimingNegative
            ) {
                anyhow::ensure!(
                    mapping.reward.is_none(),
                    "neutral, duplicate, and timing outcomes must be censored"
                );
            }
            anyhow::ensure!(
                mapping.unknown_strength == 0.0,
                "unknown-quality outcomes must not update the posterior"
            );
        }
        anyhow::ensure!(
            (1..=MAX_BANDIT_ATTRIBUTION_WINDOW_MS).contains(&self.attribution_window_ms),
            "bandit attribution window is outside the supported bound"
        );
        anyhow::ensure!(
            !self.training_manifest.dataset_digest.trim().is_empty()
                && !self.training_manifest.split_strategy.trim().is_empty(),
            "bandit training manifest is incomplete"
        );
        Ok(())
    }

    pub fn reward(&self, outcome: AttentionOutcomeKind) -> Option<&AttentionBanditRewardSpec> {
        self.reward_mapping
            .iter()
            .find(|mapping| mapping.outcome == outcome)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AttentionBanditPosteriorState {
    pub snapshot_id: String,
    pub version: u64,
    pub mean: Vec<f64>,
    pub precision: Vec<Vec<f64>>,
    pub covariance: Vec<Vec<f64>>,
    pub update_count: u64,
    pub updated_at: i64,
}

impl AttentionBanditPosteriorState {
    pub fn from_prior(snapshot: &AttentionBanditPolicySnapshot) -> Result<Self> {
        snapshot.validate()?;
        let covariance = invert_spd(&snapshot.prior_precision)?;
        let posterior = Self {
            snapshot_id: snapshot.snapshot_id.clone(),
            version: 0,
            mean: snapshot.prior_mean.clone(),
            precision: snapshot.prior_precision.clone(),
            covariance,
            update_count: 0,
            updated_at: snapshot.trained_at,
        };
        posterior.validate(snapshot)?;
        Ok(posterior)
    }

    pub fn validate(&self, snapshot: &AttentionBanditPolicySnapshot) -> Result<()> {
        anyhow::ensure!(
            self.snapshot_id == snapshot.snapshot_id,
            "posterior snapshot mismatch"
        );
        let dimension = snapshot.feature_names.len();
        anyhow::ensure!(
            self.mean.len() == dimension,
            "posterior mean dimension mismatch"
        );
        validate_square_matrix(&self.precision, dimension, "posterior precision")?;
        validate_square_matrix(&self.covariance, dimension, "posterior covariance")?;
        cholesky(&self.precision)
            .map_err(|_| anyhow::anyhow!("posterior precision is not positive definite"))?;
        cholesky(&self.covariance)
            .map_err(|_| anyhow::anyhow!("posterior covariance is not positive definite"))?;
        anyhow::ensure!(
            self.mean.iter().all(|value| value.is_finite()),
            "posterior mean contains a non-finite value"
        );
        let product = matrix_multiply(&self.precision, &self.covariance);
        for (row_index, row) in product.iter().enumerate() {
            for (column_index, value) in row.iter().enumerate() {
                let expected = if row_index == column_index { 1.0 } else { 0.0 };
                anyhow::ensure!(
                    (value - expected).abs() <= 1e-6,
                    "posterior covariance and precision are inconsistent"
                );
            }
        }
        Ok(())
    }

    pub fn uncertainty(&self) -> f64 {
        let dimension = self.covariance.len().max(1) as f64;
        self.covariance
            .iter()
            .enumerate()
            .map(|(index, row)| row[index])
            .sum::<f64>()
            / dimension
    }

    pub fn update(
        &self,
        snapshot: &AttentionBanditPolicySnapshot,
        features: &[f64],
        reward: f64,
        strength: f64,
        updated_at: i64,
    ) -> Result<Self> {
        self.validate(snapshot)?;
        anyhow::ensure!(
            features.len() == self.mean.len() && features.iter().all(|value| value.is_finite()),
            "bandit update feature vector is invalid"
        );
        anyhow::ensure!(
            reward.is_finite() && strength.is_finite() && strength > 0.0,
            "bandit update target is invalid"
        );
        let scale = strength / snapshot.observation_noise_variance;
        let mut precision = self.precision.clone();
        for row in 0..features.len() {
            for column in 0..features.len() {
                precision[row][column] += scale * features[row] * features[column];
            }
        }
        let mut natural = matrix_vector(&self.precision, &self.mean);
        for (index, feature) in features.iter().enumerate() {
            natural[index] += scale * reward * feature;
        }
        let mean = solve_spd(&precision, &natural)?;
        let covariance = invert_spd(&precision)?;
        let updated = Self {
            snapshot_id: self.snapshot_id.clone(),
            version: self.version.saturating_add(1),
            mean,
            precision,
            covariance,
            update_count: self.update_count.saturating_add(1),
            updated_at,
        };
        updated.validate(snapshot)?;
        Ok(updated)
    }
}

#[derive(Debug, Clone)]
pub struct AttentionBanditCandidate {
    pub candidate_id: String,
    pub baseline_position: usize,
    pub features: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionBanditDecisionMetadata {
    pub schema_version: u32,
    pub mode: AttentionBanditMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_model_version: Option<String>,
    pub posterior_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posterior_uncertainty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_position: Option<usize>,
    pub served_position: usize,
    pub served_propensity: f64,
    pub posterior_draw_count: u32,
    pub seed_identity: String,
    pub support: bool,
    pub exploration: bool,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionBanditHealth {
    pub mode: AttentionBanditMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_snapshot_id: Option<String>,
    pub posterior_version: u64,
    pub posterior_update_count: u64,
    pub propensity_coverage: f64,
    pub exploration_rate: f64,
    pub support_ok: bool,
    pub first_page_bounded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttentionBanditProjection {
    pub proposed_ids: Vec<String>,
    pub served_ids: Vec<String>,
    pub metadata: HashMap<String, AttentionBanditDecisionMetadata>,
    pub health: AttentionBanditHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionBanditAttributionQuality {
    VerifiedImpression,
    DecisionOnly,
    Missing,
    Mismatch,
    OutsideWindow,
}

impl AttentionBanditAttributionQuality {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::VerifiedImpression => "verified_impression",
            Self::DecisionOnly => "decision_only",
            Self::Missing => "missing",
            Self::Mismatch => "mismatch",
            Self::OutsideWindow => "outside_window",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionOutcomeAttribution {
    pub decision_id: String,
    pub candidate_id: String,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub impression_id: Option<String>,
    /// Exact frozen delivery page when feedback came from paginated canonical
    /// attention. This binding does not require a verified dwell impression.
    #[serde(default)]
    pub delivery_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionPosteriorUpdateStatus {
    Updated,
    Duplicate,
    Neutral,
    Degraded,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionPosteriorUpdateReceipt {
    pub status: AttentionPosteriorUpdateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posterior_version_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posterior_version_after: Option<u64>,
    pub attribution_quality: AttentionBanditAttributionQuality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty_before: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty_after: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_rank_before: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_rank_after: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_rank_delta: Option<i64>,
    pub rescore_scheduled: bool,
}

pub fn extract_bandit_features(
    snapshot: &AttentionBanditPolicySnapshot,
    item: &AttentionDecisionItem,
    context: &AttentionDecisionContext,
) -> Result<Vec<f64>> {
    anyhow::ensure!(
        item.routing_snapshot_id.as_deref() == Some(snapshot.routing_snapshot_id.as_str())
            && item.routing_model_version.as_deref()
                == Some(snapshot.routing_model_version.as_str())
            && item.feature_contracts.actionability_snapshot_id.as_deref()
                == Some(snapshot.actionability_snapshot_id.as_str())
            && item
                .feature_contracts
                .actionability_model_version
                .as_deref()
                == Some(snapshot.actionability_model_version.as_str())
            && item.feature_contracts.grouping_snapshot_id == snapshot.grouping_snapshot_id
            && item.feature_contracts.grouping_model_version == snapshot.grouping_model_version
            && item.feature_contracts.semantic_schema_version == snapshot.semantic_schema_version
            && item.feature_contracts.semantic_extractor_contract
                == snapshot.semantic_extractor_contract
            && item.feature_contracts.semantic_prompt_version.as_deref()
                == Some(snapshot.semantic_prompt_version.as_str())
            && item.feature_contracts.semantic_model == snapshot.semantic_model
            && item.feature_contracts.semantic_profile == snapshot.semantic_profile,
        "bandit decision-item feature contracts do not match the policy snapshot"
    );
    snapshot
        .feature_names
        .iter()
        .map(|name| match name.as_str() {
            "routing.owner_action_required_probability" => item
                .owner_action_required_probability
                .ok_or_else(|| anyhow::anyhow!("missing owner-action probability")),
            "routing.information_value_probability" => item
                .information_value_probability
                .ok_or_else(|| anyhow::anyhow!("missing information-value probability")),
            "routing.follow_up_utility" => item
                .follow_up_utility
                .ok_or_else(|| anyhow::anyhow!("missing Follow-up utility")),
            "routing.worth_a_look_utility" => item
                .worth_a_look_utility
                .ok_or_else(|| anyhow::anyhow!("missing Worth-a-look utility")),
            "routing.uncertainty" => item
                .uncertainty
                .ok_or_else(|| anyhow::anyhow!("missing routing uncertainty")),
            "group.duplicate_exposure_cost_log1p" => {
                Ok((item.cluster_size.saturating_sub(1) as f64).ln_1p())
            },
            "context.queue_size_log1p" => Ok((context.queue_size as f64).ln_1p()),
            "context.recent_impression_count_log1p" => {
                Ok((context.recent_impression_count as f64).ln_1p())
            },
            "context.recent_action_count_log1p" => Ok((context.recent_action_count as f64).ln_1p()),
            other => anyhow::bail!("unsupported bandit feature: {other}"),
        })
        .collect()
}

pub fn finite_probability_matching_rank(
    mode: AttentionBanditMode,
    canary_assigned: bool,
    first_page: bool,
    decision_id: &str,
    snapshot: &AttentionBanditPolicySnapshot,
    posterior: &AttentionBanditPosteriorState,
    candidates: &[AttentionBanditCandidate],
) -> Result<AttentionBanditProjection> {
    snapshot.validate()?;
    posterior.validate(snapshot)?;
    anyhow::ensure!(
        !decision_id.trim().is_empty(),
        "bandit decision id is empty"
    );
    let mut baseline: Vec<_> = candidates.to_vec();
    baseline.sort_by_key(|candidate| candidate.baseline_position);
    let candidate_ids: HashSet<&str> = baseline
        .iter()
        .map(|candidate| candidate.candidate_id.as_str())
        .collect();
    let baseline_positions: HashSet<usize> = baseline
        .iter()
        .map(|candidate| candidate.baseline_position)
        .collect();
    anyhow::ensure!(
        candidate_ids.len() == baseline.len()
            && baseline_positions.len() == baseline.len()
            && baseline
                .iter()
                .all(|candidate| candidate.baseline_position > 0),
        "bandit candidates require unique ids and positive unique baseline positions"
    );
    let baseline_ids: Vec<String> = baseline
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    let bounded_count = baseline.len().min(snapshot.first_page_size);
    let pool = &baseline[..bounded_count];
    let effective_slate = snapshot.slate_size.min(pool.len());
    let supported = first_page
        && mode != AttentionBanditMode::Disabled
        && effective_slate >= 1
        && pool.iter().all(|candidate| {
            candidate.features.len() == snapshot.feature_names.len()
                && candidate.features.iter().all(|value| value.is_finite())
        });
    let degradation_reason = (!supported).then(|| {
        if !first_page {
            "page_beyond_first".to_string()
        } else if mode == AttentionBanditMode::Disabled {
            "bandit_disabled".to_string()
        } else {
            "insufficient_or_incompatible_support".to_string()
        }
    });
    let mut proposed_ids = baseline_ids.clone();
    let mut proposal_propensities = HashMap::<String, f64>::new();
    let mut exploration = HashMap::<String, bool>::new();
    if supported {
        let mut remaining: Vec<&AttentionBanditCandidate> = pool.iter().collect();
        let mut chosen = Vec::new();
        for position in 0..effective_slate {
            let mut wins = vec![0_u32; remaining.len()];
            for draw in 0..snapshot.posterior_draw_count {
                let key = deployed_stream_key(
                    snapshot,
                    posterior.version,
                    decision_id,
                    position,
                    draw as usize,
                    "posterior",
                );
                let mut rng = Blake3CounterPrng::new(key);
                let weights = sample_posterior(posterior, &mut rng)?;
                let winner = remaining
                    .iter()
                    .enumerate()
                    .max_by(|(left_index, left), (right_index, right)| {
                        dot(&weights, &left.features)
                            .total_cmp(&dot(&weights, &right.features))
                            .then_with(|| right_index.cmp(left_index))
                    })
                    .map(|(index, _)| index)
                    .unwrap_or(0);
                wins[winner] = wins[winner].saturating_add(1);
            }
            let uniform = 1.0 / remaining.len() as f64;
            let probabilities: Vec<f64> = wins
                .iter()
                .map(|count| {
                    (1.0 - snapshot.exploration_floor)
                        * (*count as f64 / snapshot.posterior_draw_count as f64)
                        + snapshot.exploration_floor * uniform
                })
                .collect();
            let probability_sum = probabilities.iter().sum::<f64>();
            anyhow::ensure!(
                probabilities.iter().all(|probability| {
                    probability.is_finite() && *probability > 0.0 && *probability <= 1.0
                }) && (probability_sum - 1.0).abs() <= 1e-9,
                "bandit conditional probability distribution is invalid"
            );
            let key = deployed_stream_key(
                snapshot,
                posterior.version,
                decision_id,
                position,
                0,
                "action",
            );
            let mut rng = Blake3CounterPrng::new(key);
            let selected_index = sample_categorical(&probabilities, rng.next_f64());
            let selected = remaining.remove(selected_index);
            proposal_propensities
                .insert(selected.candidate_id.clone(), probabilities[selected_index]);
            exploration.insert(
                selected.candidate_id.clone(),
                wins[selected_index] == 0
                    || probabilities[selected_index] > 0.0
                        && wins[selected_index] < snapshot.posterior_draw_count,
            );
            chosen.push(selected.candidate_id.clone());
        }
        let chosen_set: HashSet<String> = chosen.iter().cloned().collect();
        chosen.extend(
            baseline_ids
                .iter()
                .filter(|candidate_id| !chosen_set.contains(candidate_id.as_str()))
                .cloned(),
        );
        proposed_ids = chosen;
    }
    let applied = supported && mode == AttentionBanditMode::Canary && canary_assigned;
    let served_ids = if applied {
        proposed_ids.clone()
    } else {
        baseline_ids.clone()
    };
    let proposed_position: HashMap<&str, usize> = proposed_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index + 1))
        .collect();
    let served_position: HashMap<&str, usize> = served_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index + 1))
        .collect();
    let metadata = baseline_ids
        .iter()
        .map(|candidate_id| {
            let selected_by_policy = proposed_position
                .get(candidate_id.as_str())
                .is_some_and(|position| *position <= snapshot.slate_size);
            let served_propensity = if applied && selected_by_policy {
                proposal_propensities
                    .get(candidate_id)
                    .copied()
                    .unwrap_or(0.0)
            } else if served_position
                .get(candidate_id.as_str())
                .is_some_and(|position| *position <= snapshot.slate_size)
            {
                1.0
            } else {
                0.0
            };
            (
                candidate_id.clone(),
                AttentionBanditDecisionMetadata {
                    schema_version: ATTENTION_BANDIT_SCHEMA_VERSION,
                    mode,
                    policy_snapshot_id: Some(snapshot.snapshot_id.clone()),
                    policy_model_version: Some(snapshot.model_version.clone()),
                    posterior_version: posterior.version,
                    posterior_uncertainty: Some(posterior.uncertainty()),
                    proposed_position: supported.then(|| proposed_position[candidate_id.as_str()]),
                    served_position: served_position[candidate_id.as_str()],
                    served_propensity,
                    posterior_draw_count: snapshot.posterior_draw_count,
                    seed_identity: snapshot.seed_identity.clone(),
                    support: supported,
                    exploration: applied && exploration.get(candidate_id).copied().unwrap_or(false),
                    applied,
                    degradation_reason: degradation_reason.clone(),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let selected_metadata_count = metadata
        .values()
        .filter(|item| item.served_position <= snapshot.slate_size)
        .count();
    let propensity_coverage = if selected_metadata_count == 0 {
        0.0
    } else {
        metadata
            .values()
            .filter(|item| {
                item.served_position <= snapshot.slate_size
                    && item.served_propensity.is_finite()
                    && item.served_propensity > 0.0
                    && item.served_propensity <= 1.0
            })
            .count() as f64
            / selected_metadata_count as f64
    };
    let exploration_rate = if selected_metadata_count == 0 {
        0.0
    } else {
        metadata
            .values()
            .filter(|item| item.served_position <= snapshot.slate_size && item.exploration)
            .count() as f64
            / selected_metadata_count as f64
    };
    Ok(AttentionBanditProjection {
        proposed_ids,
        served_ids,
        metadata,
        health: AttentionBanditHealth {
            mode,
            policy_snapshot_id: Some(snapshot.snapshot_id.clone()),
            posterior_version: posterior.version,
            posterior_update_count: posterior.update_count,
            propensity_coverage,
            exploration_rate,
            support_ok: supported,
            first_page_bounded: true,
            degradation_reason,
        },
    })
}

pub fn deterministic_bandit_canary_assignment(
    fraction: f64,
    snapshot: &AttentionBanditPolicySnapshot,
    principal: &str,
    workspace: &str,
    surface: AttentionSurface,
) -> bool {
    if fraction <= 0.0 {
        return false;
    }
    if fraction >= 1.0 {
        return true;
    }
    let digest = blake3::hash(
        format!(
            "{}\0{}\0{}\0{}\0{}",
            snapshot.seed_identity,
            snapshot.snapshot_id,
            principal,
            workspace,
            surface.as_str()
        )
        .as_bytes(),
    );
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest.as_bytes()[..8]);
    (u64::from_le_bytes(prefix) as f64 / u64::MAX as f64) < fraction
}

fn deployed_stream_key(
    snapshot: &AttentionBanditPolicySnapshot,
    posterior_version: u64,
    decision_id: &str,
    position: usize,
    draw: usize,
    purpose: &str,
) -> [u8; 32] {
    *blake3::hash(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}\0{}",
            snapshot.seed_contract,
            snapshot.seed_identity,
            snapshot.snapshot_id,
            posterior_version,
            decision_id,
            position,
            format_args!("{purpose}:{draw}")
        )
        .as_bytes(),
    )
    .as_bytes()
}

/// Fully specified, release-independent PRNG. Each word is the first
/// little-endian u64 of keyed BLAKE3 over a fixed domain plus a little-endian
/// counter; f64 values use the high 53 bits and divide by 2^53.
struct Blake3CounterPrng {
    key: [u8; 32],
    counter: u64,
}

impl Blake3CounterPrng {
    const fn new(key: [u8; 32]) -> Self {
        Self { key, counter: 0 }
    }

    fn next_u64(&mut self) -> u64 {
        let mut hasher = blake3::Hasher::new_keyed(&self.key);
        hasher.update(b"attention-bandit-prng-v1\0");
        hasher.update(&self.counter.to_le_bytes());
        self.counter = self.counter.saturating_add(1);
        let digest = hasher.finalize();
        let mut word = [0_u8; 8];
        word.copy_from_slice(&digest.as_bytes()[..8]);
        u64::from_le_bytes(word)
    }

    fn next_f64(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) / ((1_u64 << 53) as f64)
    }
}

fn sample_posterior(
    posterior: &AttentionBanditPosteriorState,
    rng: &mut Blake3CounterPrng,
) -> Result<Vec<f64>> {
    let lower = cholesky(&posterior.covariance)?;
    let mut normals = Vec::with_capacity(posterior.mean.len());
    while normals.len() < posterior.mean.len() {
        let first = rng.next_f64().max(f64::MIN_POSITIVE);
        let second = rng.next_f64();
        let radius = (-2.0 * first.ln()).sqrt();
        let angle = std::f64::consts::TAU * second;
        normals.push(radius * angle.cos());
        if normals.len() < posterior.mean.len() {
            normals.push(radius * angle.sin());
        }
    }
    let perturbation = matrix_vector(&lower, &normals);
    Ok(posterior
        .mean
        .iter()
        .zip(perturbation)
        .map(|(mean, noise)| mean + noise)
        .collect())
}

fn sample_categorical(probabilities: &[f64], draw: f64) -> usize {
    let mut cumulative = 0.0;
    for (index, probability) in probabilities.iter().enumerate() {
        cumulative += probability;
        if draw < cumulative || index + 1 == probabilities.len() {
            return index;
        }
    }
    probabilities.len().saturating_sub(1)
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn validate_square_matrix(matrix: &[Vec<f64>], dimension: usize, label: &str) -> Result<()> {
    anyhow::ensure!(matrix.len() == dimension, "{label} dimension mismatch");
    anyhow::ensure!(
        matrix
            .iter()
            .all(|row| row.len() == dimension && row.iter().all(|value| value.is_finite())),
        "{label} is not a finite square matrix"
    );
    for row in 0..dimension {
        for column in 0..dimension {
            anyhow::ensure!(
                (matrix[row][column] - matrix[column][row]).abs() <= 1e-9,
                "{label} is not symmetric"
            );
        }
    }
    Ok(())
}

fn cholesky(matrix: &[Vec<f64>]) -> Result<Vec<Vec<f64>>> {
    let dimension = matrix.len();
    let mut lower = vec![vec![0.0; dimension]; dimension];
    for row in 0..dimension {
        for column in 0..=row {
            let sum = (0..column)
                .map(|index| lower[row][index] * lower[column][index])
                .sum::<f64>();
            if row == column {
                let diagonal = matrix[row][row] - sum;
                anyhow::ensure!(
                    diagonal.is_finite() && diagonal > 1e-12,
                    "matrix is not positive definite"
                );
                lower[row][column] = diagonal.sqrt();
            } else {
                lower[row][column] = (matrix[row][column] - sum) / lower[column][column];
            }
        }
    }
    Ok(lower)
}

fn solve_spd(matrix: &[Vec<f64>], target: &[f64]) -> Result<Vec<f64>> {
    let lower = cholesky(matrix)?;
    let dimension = lower.len();
    anyhow::ensure!(
        target.len() == dimension,
        "linear target dimension mismatch"
    );
    let mut forward = vec![0.0; dimension];
    for row in 0..dimension {
        let sum = (0..row)
            .map(|column| lower[row][column] * forward[column])
            .sum::<f64>();
        forward[row] = (target[row] - sum) / lower[row][row];
    }
    let mut solution = vec![0.0; dimension];
    for row in (0..dimension).rev() {
        let sum = ((row + 1)..dimension)
            .map(|column| lower[column][row] * solution[column])
            .sum::<f64>();
        solution[row] = (forward[row] - sum) / lower[row][row];
    }
    Ok(solution)
}

fn invert_spd(matrix: &[Vec<f64>]) -> Result<Vec<Vec<f64>>> {
    let dimension = matrix.len();
    let mut inverse = vec![vec![0.0; dimension]; dimension];
    for column in 0..dimension {
        let mut basis = vec![0.0; dimension];
        basis[column] = 1.0;
        let solution = solve_spd(matrix, &basis)?;
        for row in 0..dimension {
            inverse[row][column] = solution[row];
        }
    }
    Ok(inverse)
}

fn matrix_vector(matrix: &[Vec<f64>], vector: &[f64]) -> Vec<f64> {
    matrix.iter().map(|row| dot(row, vector)).collect()
}

fn matrix_multiply(left: &[Vec<f64>], right: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let dimension = left.len();
    let mut product = vec![vec![0.0; dimension]; dimension];
    for row in 0..dimension {
        for column in 0..dimension {
            product[row][column] = (0..dimension)
                .map(|index| left[row][index] * right[index][column])
                .sum();
        }
    }
    product
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn snapshot() -> AttentionBanditPolicySnapshot {
        AttentionBanditPolicySnapshot {
            snapshot_id: "bandit-v1".to_string(),
            model_version: "bayesian-linear-v1".to_string(),
            feature_contract: ATTENTION_BANDIT_FEATURE_CONTRACT.to_string(),
            routing_feature_contract: ATTENTION_ROUTING_FEATURE_CONTRACT.to_string(),
            routing_snapshot_id: "route-v1".to_string(),
            routing_model_version: "route-model-v1".to_string(),
            actionability_feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
            actionability_snapshot_id: "action-v1".to_string(),
            actionability_model_version: "action-model-v1".to_string(),
            grouping_feature_contract: ATTENTION_PAIR_FEATURE_CONTRACT.to_string(),
            grouping_snapshot_id: None,
            grouping_model_version: None,
            semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            semantic_prompt_version: "1.1.0".to_string(),
            semantic_model: None,
            semantic_profile: None,
            feature_names: vec!["context.queue_size_log1p".to_string()],
            prior_mean: vec![0.0],
            prior_precision: vec![vec![1.0]],
            observation_noise_variance: 1.0,
            posterior_draw_count: 64,
            exploration_floor: 0.05,
            first_page_size: 3,
            slate_size: 2,
            seed_contract: ATTENTION_BANDIT_SEED_CONTRACT.to_string(),
            seed_identity: "experiment-v1".to_string(),
            reward_mapping: [
                (AttentionOutcomeKind::Useful, Some(1.0)),
                (AttentionOutcomeKind::ActionCompleted, Some(1.0)),
                (AttentionOutcomeKind::Irrelevant, Some(-1.0)),
                (AttentionOutcomeKind::NotActionable, Some(-1.0)),
                (AttentionOutcomeKind::Obsolete, Some(-1.0)),
                (AttentionOutcomeKind::NotOwner, Some(-1.0)),
                (AttentionOutcomeKind::DuplicateOf, None),
                (AttentionOutcomeKind::NeutralSeen, None),
                (AttentionOutcomeKind::TimingNegative, None),
            ]
            .into_iter()
            .map(|(outcome, reward)| AttentionBanditRewardSpec {
                outcome,
                reward,
                strong_strength: 1.0,
                weak_strength: 0.5,
                unknown_strength: 0.0,
            })
            .collect(),
            attribution_window_ms: 86_400_000,
            require_verified_impression: true,
            trained_at: 1,
            training_manifest: AttentionBanditTrainingManifest {
                dataset_digest: "fixture".to_string(),
                data_cutoff_at: 1,
                split_strategy: "grouped_temporal".to_string(),
                group_keys: vec!["session".to_string()],
                metrics: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn finite_policy_replays_exactly_and_records_positive_conditional_propensities() {
        let snapshot = snapshot();
        let posterior = AttentionBanditPosteriorState::from_prior(&snapshot).unwrap();
        let candidates = (0..3)
            .map(|index| AttentionBanditCandidate {
                candidate_id: format!("candidate-{index}"),
                baseline_position: index + 1,
                features: vec![index as f64],
            })
            .collect::<Vec<_>>();
        let first = finite_probability_matching_rank(
            AttentionBanditMode::Canary,
            true,
            true,
            "decision-1",
            &snapshot,
            &posterior,
            &candidates,
        )
        .unwrap();
        let replay = finite_probability_matching_rank(
            AttentionBanditMode::Canary,
            true,
            true,
            "decision-1",
            &snapshot,
            &posterior,
            &candidates,
        )
        .unwrap();
        assert_eq!(first, replay);
        assert!(first
            .metadata
            .values()
            .filter(|item| item.served_position <= snapshot.slate_size)
            .all(|item| item.served_propensity > 0.0 && item.served_propensity <= 1.0));
    }

    #[test]
    fn a_short_lane_still_has_support_when_slate_exceeds_pool() {
        let mut snapshot = snapshot();
        snapshot.slate_size = 2;
        snapshot.first_page_size = 3;
        let posterior = AttentionBanditPosteriorState::from_prior(&snapshot).unwrap();
        let candidates = vec![AttentionBanditCandidate {
            candidate_id: "candidate-0".to_string(),
            baseline_position: 1,
            features: vec![0.0],
        }];
        let projection = finite_probability_matching_rank(
            AttentionBanditMode::Canary,
            true,
            true,
            "decision-1",
            &snapshot,
            &posterior,
            &candidates,
        )
        .unwrap();
        assert_eq!(projection.served_ids, vec!["candidate-0".to_string()]);
        assert!(projection.health.support_ok);
    }

    #[test]
    fn shadow_serves_baseline_with_propensity_one() {
        let snapshot = snapshot();
        let posterior = AttentionBanditPosteriorState::from_prior(&snapshot).unwrap();
        let candidates = (0..3)
            .map(|index| AttentionBanditCandidate {
                candidate_id: format!("candidate-{index}"),
                baseline_position: index + 1,
                features: vec![index as f64],
            })
            .collect::<Vec<_>>();
        let projection = finite_probability_matching_rank(
            AttentionBanditMode::Shadow,
            false,
            true,
            "decision-1",
            &snapshot,
            &posterior,
            &candidates,
        )
        .unwrap();
        assert_eq!(
            projection.served_ids,
            vec!["candidate-0", "candidate-1", "candidate-2"]
        );
        assert!(projection
            .metadata
            .values()
            .filter(|item| item.served_position <= snapshot.slate_size)
            .all(|item| item.served_propensity == 1.0 && !item.applied));
    }

    #[test]
    fn censored_outcomes_cannot_become_reward_targets() {
        let mut snapshot = snapshot();
        snapshot
            .reward_mapping
            .iter_mut()
            .find(|mapping| mapping.outcome == AttentionOutcomeKind::NeutralSeen)
            .unwrap()
            .reward = Some(-1.0);
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn observation_noise_variance_is_bounded_away_from_numerical_instability() {
        let mut policy = snapshot();
        policy.observation_noise_variance = MIN_BANDIT_OBSERVATION_NOISE_VARIANCE / 10.0;
        assert!(policy.validate().is_err());
        policy.observation_noise_variance = MAX_BANDIT_OBSERVATION_NOISE_VARIANCE * 10.0;
        assert!(policy.validate().is_err());
    }

    #[test]
    fn posterior_update_is_versioned_and_precision_covariance_remain_consistent() {
        let snapshot = snapshot();
        let prior = AttentionBanditPosteriorState::from_prior(&snapshot).unwrap();
        let updated = prior.update(&snapshot, &[1.0], 1.0, 1.0, 2).unwrap();
        assert_eq!(updated.version, 1);
        assert!(updated.uncertainty() < prior.uncertainty());
        updated.validate(&snapshot).unwrap();
    }
}
