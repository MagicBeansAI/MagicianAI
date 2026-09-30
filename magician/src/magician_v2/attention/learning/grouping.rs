//! Slice-3 calibrated pair inference and conservative evidence-preserving grouping.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    AttentionLabelQuality, AttentionSurface, ChannelAttentionSemanticEnvelope, SemanticEmbedding,
    ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT, ATTENTION_SEMANTIC_SCHEMA_VERSION,
};

pub const ATTENTION_PAIR_FEATURE_CONTRACT: &str = "attention_pair_features_v1";
pub const ATTENTION_PAIR_LABEL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AttentionPairLabelKind {
    SameUnderlyingItem,
    SameObligation,
    NotDuplicate,
}

impl AttentionPairLabelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SameUnderlyingItem => "same_underlying_item",
            Self::SameObligation => "same_obligation",
            Self::NotDuplicate => "not_duplicate",
        }
    }
}

impl std::str::FromStr for AttentionPairLabelKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "same_underlying_item" => Ok(Self::SameUnderlyingItem),
            "same_obligation" => Ok(Self::SameObligation),
            "not_duplicate" => Ok(Self::NotDuplicate),
            other => anyhow::bail!("unknown attention pair label: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AttentionPairLabelSource {
    Owner,
    ExactSourceIdentity,
    ReplayAudit,
    ModelSuggestion,
}

impl AttentionPairLabelSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::ExactSourceIdentity => "exact_source_identity",
            Self::ReplayAudit => "replay_audit",
            Self::ModelSuggestion => "model_suggestion",
        }
    }
}

impl std::str::FromStr for AttentionPairLabelSource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "owner" => Ok(Self::Owner),
            "exact_source_identity" => Ok(Self::ExactSourceIdentity),
            "replay_audit" => Ok(Self::ReplayAudit),
            "model_suggestion" => Ok(Self::ModelSuggestion),
            other => anyhow::bail!("unknown attention pair label source: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AttentionPairCandidateRef {
    pub candidate_id: String,
    pub source_revision: Option<String>,
}

impl AttentionPairCandidateRef {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.candidate_id.trim().is_empty() && self.candidate_id.chars().count() <= 500,
            "attention pair candidate_id must contain 1..=500 characters"
        );
        anyhow::ensure!(
            self.source_revision.as_deref().is_none_or(
                |revision| !revision.trim().is_empty() && revision.chars().count() <= 500
            ),
            "attention pair source_revision must contain 1..=500 characters"
        );
        Ok(())
    }

    fn order_key(&self) -> (&str, &str) {
        (
            self.candidate_id.as_str(),
            self.source_revision.as_deref().unwrap_or_default(),
        )
    }
}

pub fn canonical_pair(
    left: AttentionPairCandidateRef,
    right: AttentionPairCandidateRef,
) -> Result<(AttentionPairCandidateRef, AttentionPairCandidateRef)> {
    left.validate()?;
    right.validate()?;
    anyhow::ensure!(left != right, "attention pair candidates must be distinct");
    Ok(if left.order_key() <= right.order_key() {
        (left, right)
    } else {
        (right, left)
    })
}

#[derive(Debug, Clone)]
pub struct RecordAttentionPairLabel {
    pub event_id: String,
    pub surface: AttentionSurface,
    pub left: AttentionPairCandidateRef,
    pub right: AttentionPairCandidateRef,
    pub label: AttentionPairLabelKind,
    pub source: AttentionPairLabelSource,
    pub label_quality: AttentionLabelQuality,
    pub confidence: f64,
    pub occurred_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionPairFeedbackReceipt {
    pub pair_label_id: String,
    pub inserted: bool,
    pub label: AttentionPairLabelKind,
    pub canonical_left_id: String,
    pub canonical_right_id: String,
    pub affected_cluster_ids: Vec<String>,
    pub grouping_generation: u64,
    pub recomputed: bool,
}

#[derive(Debug, Clone)]
pub struct PersistedPairEvidence {
    pub left: AttentionPairCandidateRef,
    pub right: AttentionPairCandidateRef,
    pub label: AttentionPairLabelKind,
    pub source: AttentionPairLabelSource,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default)]
pub struct GroupingFeatureInput {
    pub semantic: Option<ChannelAttentionSemanticEnvelope>,
    pub embedding: Option<SemanticEmbedding>,
    pub exact_source_identity: Option<String>,
    pub sender_identity: Option<String>,
    pub account_identity: Option<String>,
    pub event_at_ms: Option<i64>,
    pub entity_keys: Vec<String>,
    pub action_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GroupingCandidate {
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub baseline_rank: usize,
    pub learned_rank: usize,
    pub features: GroupingFeatureInput,
}

impl GroupingCandidate {
    pub fn candidate_ref(&self) -> AttentionPairCandidateRef {
        AttentionPairCandidateRef {
            candidate_id: self.candidate_id.clone(),
            source_revision: self.source_revision.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionPairModelHead {
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    pub platt_a: f64,
    pub platt_b: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionPairTrainingManifest {
    pub dataset_digest: String,
    pub data_cutoff_at: i64,
    pub split_strategy: String,
    pub group_keys: Vec<String>,
    pub audited_pair_count: usize,
    pub false_merge_count: usize,
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionPairModelSnapshot {
    pub snapshot_id: String,
    pub model_version: String,
    pub feature_contract: String,
    pub semantic_schema_version: u32,
    pub semantic_extractor_contract: String,
    pub semantic_prompt_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_contract: Option<String>,
    pub feature_names: Vec<String>,
    pub same_underlying_item: AttentionPairModelHead,
    pub same_obligation: AttentionPairModelHead,
    /// Selected from the audited held-out pair set. Serving has no separate
    /// hidden merge weight or threshold.
    pub audited_merge_threshold: f64,
    pub trained_at: i64,
    pub training_manifest: AttentionPairTrainingManifest,
}

impl AttentionPairModelSnapshot {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.snapshot_id.trim().is_empty(),
            "pair snapshot id is empty"
        );
        anyhow::ensure!(
            !self.model_version.trim().is_empty(),
            "pair model version is empty"
        );
        anyhow::ensure!(
            self.feature_contract == ATTENTION_PAIR_FEATURE_CONTRACT,
            "pair feature contract is incompatible"
        );
        anyhow::ensure!(
            self.semantic_schema_version == ATTENTION_SEMANTIC_SCHEMA_VERSION,
            "pair semantic schema is incompatible"
        );
        anyhow::ensure!(
            self.semantic_extractor_contract == ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
            "pair semantic extractor contract is incompatible"
        );
        anyhow::ensure!(
            !self.semantic_prompt_version.trim().is_empty(),
            "pair semantic prompt version is empty"
        );
        anyhow::ensure!(
            self.embedding_contract
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty()),
            "pair embedding contract is empty"
        );
        anyhow::ensure!(
            (0.5..=1.0).contains(&self.audited_merge_threshold),
            "pair audited merge threshold must be within 0.5..=1.0"
        );
        anyhow::ensure!(
            !self.feature_names.is_empty(),
            "pair snapshot has no features"
        );
        let mut unique = HashSet::new();
        for feature in &self.feature_names {
            anyhow::ensure!(unique.insert(feature), "duplicate pair feature: {feature}");
            anyhow::ensure!(
                supported_feature(feature),
                "unsupported pair feature: {feature}"
            );
        }
        for head in [&self.same_underlying_item, &self.same_obligation] {
            anyhow::ensure!(
                head.coefficients.len() == self.feature_names.len(),
                "pair model coefficient dimension mismatch"
            );
            anyhow::ensure!(
                head.intercept.is_finite() && head.platt_a.is_finite() && head.platt_b.is_finite(),
                "pair model parameters are invalid"
            );
            anyhow::ensure!(
                head.coefficients.iter().all(|value| value.is_finite()),
                "pair model coefficient is non-finite"
            );
        }
        anyhow::ensure!(
            !self.training_manifest.dataset_digest.trim().is_empty(),
            "pair training dataset digest is empty"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionPairInference {
    pub same_underlying_item_probability: f64,
    pub same_obligation_probability: f64,
    pub merge_probability: f64,
}

pub fn infer_pair(
    snapshot: &AttentionPairModelSnapshot,
    left: &GroupingCandidate,
    right: &GroupingCandidate,
) -> Result<Option<AttentionPairInference>> {
    snapshot.validate()?;
    let values = pair_feature_values(snapshot, left, right);
    if !values.values().any(|value| *value != 0.0) {
        return Ok(None);
    }
    let underlying = infer_head(snapshot, &snapshot.same_underlying_item, &values);
    let obligation = infer_head(snapshot, &snapshot.same_obligation, &values);
    Ok(Some(AttentionPairInference {
        same_underlying_item_probability: underlying,
        same_obligation_probability: obligation,
        merge_probability: underlying.max(obligation),
    }))
}

fn infer_head(
    snapshot: &AttentionPairModelSnapshot,
    head: &AttentionPairModelHead,
    values: &BTreeMap<String, f64>,
) -> f64 {
    let linear = snapshot
        .feature_names
        .iter()
        .zip(&head.coefficients)
        .fold(head.intercept, |total, (name, coefficient)| {
            total + coefficient * values.get(name).copied().unwrap_or(0.0)
        });
    sigmoid(head.platt_a * linear + head.platt_b)
}

fn pair_feature_values(
    snapshot: &AttentionPairModelSnapshot,
    left: &GroupingCandidate,
    right: &GroupingCandidate,
) -> BTreeMap<String, f64> {
    let mut values = BTreeMap::new();
    if let (Some(left_embedding), Some(right_embedding), Some(contract)) = (
        left.features.embedding.as_ref(),
        right.features.embedding.as_ref(),
        snapshot.embedding_contract.as_deref(),
    ) {
        if left_embedding.contract == contract
            && right_embedding.contract == contract
            && left_embedding.vector.len() == right_embedding.vector.len()
        {
            if let Some(similarity) =
                cosine_similarity(&left_embedding.vector, &right_embedding.vector)
            {
                values.insert("embedding.cosine_similarity".to_string(), similarity);
            }
        }
    }
    if compatible_semantic(snapshot, left) && compatible_semantic(snapshot, right) {
        let same_campaign = left
            .features
            .semantic
            .as_ref()
            .and_then(|semantic| semantic.features.as_ref())
            .and_then(|features| features.campaign_or_event_identity.as_deref())
            .zip(
                right
                    .features
                    .semantic
                    .as_ref()
                    .and_then(|semantic| semantic.features.as_ref())
                    .and_then(|features| features.campaign_or_event_identity.as_deref()),
            )
            .is_some_and(|(left, right)| left == right);
        values.insert(
            "semantic.same_campaign_event".to_string(),
            if same_campaign { 1.0 } else { 0.0 },
        );
    }
    insert_same(
        &mut values,
        "source.exact_identity",
        &left.features.exact_source_identity,
        &right.features.exact_source_identity,
    );
    insert_same(
        &mut values,
        "sender.same",
        &left.features.sender_identity,
        &right.features.sender_identity,
    );
    insert_same(
        &mut values,
        "account.same",
        &left.features.account_identity,
        &right.features.account_identity,
    );
    insert_same(
        &mut values,
        "action.same",
        &left.features.action_key,
        &right.features.action_key,
    );
    if let (Some(left_at), Some(right_at)) = (left.features.event_at_ms, right.features.event_at_ms)
    {
        let days = left_at.abs_diff(right_at) as f64 / 86_400_000.0;
        values.insert("time.distance_days_log1p".to_string(), days.ln_1p());
    }
    if !left.features.entity_keys.is_empty() && !right.features.entity_keys.is_empty() {
        let left_entities: BTreeSet<_> = left.features.entity_keys.iter().collect();
        let right_entities: BTreeSet<_> = right.features.entity_keys.iter().collect();
        let union = left_entities.union(&right_entities).count();
        if union > 0 {
            values.insert(
                "entity.jaccard".to_string(),
                left_entities.intersection(&right_entities).count() as f64 / union as f64,
            );
        }
    }
    values
}

fn insert_same(
    values: &mut BTreeMap<String, f64>,
    name: &str,
    left: &Option<String>,
    right: &Option<String>,
) {
    if let (Some(left), Some(right)) = (left.as_deref(), right.as_deref()) {
        values.insert(name.to_string(), if left == right { 1.0 } else { 0.0 });
    }
}

fn compatible_semantic(
    snapshot: &AttentionPairModelSnapshot,
    candidate: &GroupingCandidate,
) -> bool {
    candidate
        .features
        .semantic
        .as_ref()
        .is_some_and(|semantic| {
            semantic.is_compatible(candidate.source_revision.as_deref())
                && semantic.schema_version == snapshot.semantic_schema_version
                && semantic.extractor_contract == snapshot.semantic_extractor_contract
                && semantic.prompt_version == snapshot.semantic_prompt_version
                && semantic.model == snapshot.semantic_model
                && semantic.profile == snapshot.semantic_profile
        })
}

fn supported_feature(name: &str) -> bool {
    matches!(
        name,
        "embedding.cosine_similarity"
            | "semantic.same_campaign_event"
            | "source.exact_identity"
            | "sender.same"
            | "account.same"
            | "time.distance_days_log1p"
            | "entity.jaccard"
            | "action.same"
    )
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f64> {
    let (mut dot, mut left_norm, mut right_norm) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (left, right) in left.iter().zip(right) {
        if !left.is_finite() || !right.is_finite() {
            return None;
        }
        let (left, right) = (f64::from(*left), f64::from(*right));
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        return None;
    }
    Some((dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0))
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionGroupingMetadata {
    pub cluster_id: String,
    pub representative_id: String,
    pub is_representative: bool,
    pub member_count: usize,
    pub related_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_probability: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionCluster {
    pub cluster_id: String,
    pub representative_id: String,
    pub member_ids: Vec<String>,
    pub merge_probability: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AttentionGroupingProjection {
    pub clusters: Vec<AttentionCluster>,
    pub metadata: HashMap<String, AttentionGroupingMetadata>,
    pub representative_ids: Vec<String>,
    pub scored_pair_total: usize,
    pub cannot_link_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionGroupingHealth {
    pub candidate_total: usize,
    pub member_total: usize,
    pub cluster_total: usize,
    pub representative_total: usize,
    pub collapsed_member_total: usize,
    pub scored_pair_total: usize,
    pub cannot_link_total: usize,
    pub fallback_ungrouped_total: usize,
    pub totals_reconcile: bool,
    pub pair_evaluation_budget: u64,
    pub required_pair_evaluations: u64,
    pub budget_exceeded: bool,
}

impl AttentionGroupingHealth {
    pub fn from_projection(
        candidate_total: usize,
        projection: &AttentionGroupingProjection,
        fallback_ungrouped_total: usize,
        pair_evaluation_budget: u64,
        required_pair_evaluations: u64,
        budget_exceeded: bool,
    ) -> Self {
        let member_total: usize = projection
            .clusters
            .iter()
            .map(|cluster| cluster.member_ids.len())
            .sum();
        let cluster_total = projection.clusters.len();
        let representative_total = projection.representative_ids.len();
        let collapsed_member_total = member_total.saturating_sub(representative_total);
        Self {
            candidate_total,
            member_total,
            cluster_total,
            representative_total,
            collapsed_member_total,
            scored_pair_total: projection.scored_pair_total,
            cannot_link_total: projection.cannot_link_total,
            fallback_ungrouped_total,
            totals_reconcile: member_total == candidate_total
                && projection.metadata.len() == candidate_total
                && cluster_total == representative_total
                && representative_total <= member_total,
            pair_evaluation_budget,
            required_pair_evaluations,
            budget_exceeded,
        }
    }
}

/// Complete unordered-pair count with overflow-safe saturation. A saturated
/// value necessarily exceeds any practical finite runtime budget.
pub fn required_pair_evaluations(candidate_count: usize) -> u64 {
    let count = candidate_count as u128;
    let required = count.saturating_mul(count.saturating_sub(1)) / 2;
    required.min(u128::from(u64::MAX)) as u64
}

pub fn singleton_grouping(candidates: &[GroupingCandidate]) -> AttentionGroupingProjection {
    let clusters: Vec<_> = candidates
        .iter()
        .map(|candidate| AttentionCluster {
            cluster_id: stable_cluster_id(&[candidate.candidate_ref()]),
            representative_id: candidate.candidate_id.clone(),
            member_ids: vec![candidate.candidate_id.clone()],
            merge_probability: None,
        })
        .collect();
    projection_from_clusters(candidates, clusters, None, None, 0, 0)
}

pub fn singleton_grouping_metadata(
    candidate: AttentionPairCandidateRef,
) -> AttentionGroupingMetadata {
    AttentionGroupingMetadata {
        cluster_id: stable_cluster_id(std::slice::from_ref(&candidate)),
        representative_id: candidate.candidate_id.clone(),
        is_representative: true,
        member_count: 1,
        related_count: 0,
        model_version: None,
        snapshot_id: None,
        merge_probability: None,
    }
}

pub fn cluster_candidates(
    snapshot: &AttentionPairModelSnapshot,
    candidates: &[GroupingCandidate],
    evidence: &[PersistedPairEvidence],
) -> Result<AttentionGroupingProjection> {
    snapshot.validate()?;
    let refs: Vec<_> = candidates
        .iter()
        .map(GroupingCandidate::candidate_ref)
        .collect();
    let index_by_ref: HashMap<_, _> = refs
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, candidate)| (candidate, index))
        .collect();
    let mut cannot_links = HashSet::new();
    let mut forced_links = HashSet::new();
    for label in evidence {
        let Some(left) = index_by_ref.get(&label.left).copied() else {
            continue;
        };
        let Some(right) = index_by_ref.get(&label.right).copied() else {
            continue;
        };
        let pair = ordered_index_pair(left, right);
        if label.source == AttentionPairLabelSource::Owner
            && label.label == AttentionPairLabelKind::NotDuplicate
        {
            cannot_links.insert(pair);
        } else if label.source != AttentionPairLabelSource::ModelSuggestion
            && matches!(
                label.label,
                AttentionPairLabelKind::SameUnderlyingItem | AttentionPairLabelKind::SameObligation
            )
        {
            forced_links.insert(pair);
        }
    }

    #[derive(Debug)]
    struct Edge {
        left: usize,
        right: usize,
        probability: f64,
    }
    let mut edges = Vec::new();
    let mut scored_pair_total = 0;
    for left in 0..candidates.len() {
        for right in (left + 1)..candidates.len() {
            let pair = (left, right);
            let exact_source = candidates[left]
                .features
                .exact_source_identity
                .as_ref()
                .zip(candidates[right].features.exact_source_identity.as_ref())
                .is_some_and(|(left, right)| left == right);
            let forced = forced_links.contains(&pair);
            let inference = infer_pair(snapshot, &candidates[left], &candidates[right])?;
            scored_pair_total += usize::from(inference.is_some());
            let probability = if exact_source || forced {
                1.0
            } else {
                inference
                    .map(|inference| inference.merge_probability)
                    .unwrap_or(0.0)
            };
            if exact_source || forced || probability >= snapshot.audited_merge_threshold {
                edges.push(Edge {
                    left,
                    right,
                    probability,
                });
            }
        }
    }
    edges.sort_by(|left, right| {
        right
            .probability
            .total_cmp(&left.probability)
            .then_with(|| {
                refs[left.left]
                    .order_key()
                    .cmp(&refs[right.left].order_key())
            })
            .then_with(|| {
                refs[left.right]
                    .order_key()
                    .cmp(&refs[right.right].order_key())
            })
    });

    let mut components: Vec<BTreeSet<usize>> = (0..candidates.len())
        .map(|index| BTreeSet::from([index]))
        .collect();
    let mut component_of: Vec<usize> = (0..candidates.len()).collect();
    let mut component_probability = vec![None::<f64>; candidates.len()];
    for edge in edges {
        let left_component = component_of[edge.left];
        let right_component = component_of[edge.right];
        if left_component == right_component {
            continue;
        }
        let blocked = components[left_component].iter().any(|left| {
            components[right_component]
                .iter()
                .any(|right| cannot_links.contains(&ordered_index_pair(*left, *right)))
        });
        if blocked {
            continue;
        }
        let (keep, remove) = if left_component < right_component {
            (left_component, right_component)
        } else {
            (right_component, left_component)
        };
        let removed = std::mem::take(&mut components[remove]);
        for index in removed {
            components[keep].insert(index);
            component_of[index] = keep;
        }
        component_probability[keep] = Some(
            component_probability[keep]
                .unwrap_or(0.0)
                .max(component_probability[remove].unwrap_or(0.0))
                .max(edge.probability),
        );
    }

    let mut clusters = Vec::new();
    for (component, members) in components.into_iter().enumerate() {
        if members.is_empty() {
            continue;
        }
        let mut members: Vec<_> = members.into_iter().collect();
        members.sort_by(|left, right| representative_cmp(&candidates[*left], &candidates[*right]));
        let member_refs: Vec<_> = members.iter().map(|index| refs[*index].clone()).collect();
        clusters.push(AttentionCluster {
            cluster_id: stable_cluster_id(&member_refs),
            representative_id: candidates[members[0]].candidate_id.clone(),
            member_ids: members
                .iter()
                .map(|index| candidates[*index].candidate_id.clone())
                .collect(),
            merge_probability: component_probability[component],
        });
    }
    clusters.sort_by(|left, right| {
        let left_candidate = candidates
            .iter()
            .find(|candidate| candidate.candidate_id == left.representative_id)
            .expect("cluster representative exists");
        let right_candidate = candidates
            .iter()
            .find(|candidate| candidate.candidate_id == right.representative_id)
            .expect("cluster representative exists");
        representative_cmp(left_candidate, right_candidate)
    });
    Ok(projection_from_clusters(
        candidates,
        clusters,
        Some(snapshot.model_version.as_str()),
        Some(snapshot.snapshot_id.as_str()),
        scored_pair_total,
        cannot_links.len(),
    ))
}

fn projection_from_clusters(
    candidates: &[GroupingCandidate],
    clusters: Vec<AttentionCluster>,
    model_version: Option<&str>,
    snapshot_id: Option<&str>,
    scored_pair_total: usize,
    cannot_link_total: usize,
) -> AttentionGroupingProjection {
    let mut metadata = HashMap::new();
    for cluster in &clusters {
        for member_id in &cluster.member_ids {
            metadata.insert(
                member_id.clone(),
                AttentionGroupingMetadata {
                    cluster_id: cluster.cluster_id.clone(),
                    representative_id: cluster.representative_id.clone(),
                    is_representative: member_id == &cluster.representative_id,
                    member_count: cluster.member_ids.len(),
                    related_count: cluster.member_ids.len().saturating_sub(1),
                    model_version: model_version.map(str::to_string),
                    snapshot_id: snapshot_id.map(str::to_string),
                    merge_probability: cluster.merge_probability,
                },
            );
        }
    }
    let representative_ids = clusters
        .iter()
        .map(|cluster| cluster.representative_id.clone())
        .collect();
    debug_assert_eq!(metadata.len(), candidates.len());
    AttentionGroupingProjection {
        clusters,
        metadata,
        representative_ids,
        scored_pair_total,
        cannot_link_total,
    }
}

fn representative_cmp(left: &GroupingCandidate, right: &GroupingCandidate) -> std::cmp::Ordering {
    left.learned_rank
        .cmp(&right.learned_rank)
        .then_with(|| left.baseline_rank.cmp(&right.baseline_rank))
        .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        .then_with(|| left.source_revision.cmp(&right.source_revision))
}

fn ordered_index_pair(left: usize, right: usize) -> (usize, usize) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn stable_cluster_id(members: &[AttentionPairCandidateRef]) -> String {
    let mut members = members.to_vec();
    members.sort_by(|left, right| left.order_key().cmp(&right.order_key()));
    let mut digest = blake3::Hasher::new();
    for member in members {
        digest.update(member.candidate_id.as_bytes());
        digest.update(b"\x1e");
        if let Some(revision) = member.source_revision {
            digest.update(revision.as_bytes());
        }
        digest.update(b"\x1f");
    }
    format!("attention-cluster-{}", digest.finalize().to_hex())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn snapshot() -> AttentionPairModelSnapshot {
        let features = vec!["source.exact_identity".to_string()];
        let head = AttentionPairModelHead {
            coefficients: vec![10.0],
            intercept: -5.0,
            platt_a: 1.0,
            platt_b: 0.0,
        };
        AttentionPairModelSnapshot {
            snapshot_id: "pair-v1".to_string(),
            model_version: "pair-logistic-v1".to_string(),
            feature_contract: ATTENTION_PAIR_FEATURE_CONTRACT.to_string(),
            semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            semantic_prompt_version: "1.1.0".to_string(),
            semantic_model: None,
            semantic_profile: None,
            embedding_contract: None,
            feature_names: features,
            same_underlying_item: head.clone(),
            same_obligation: head,
            audited_merge_threshold: 0.99,
            trained_at: 1,
            training_manifest: AttentionPairTrainingManifest {
                dataset_digest: "fixture".to_string(),
                data_cutoff_at: 1,
                split_strategy: "grouped_temporal".to_string(),
                group_keys: vec!["source".to_string()],
                audited_pair_count: 3,
                false_merge_count: 0,
                metrics: BTreeMap::new(),
            },
        }
    }

    fn candidate(id: &str, source: &str, rank: usize) -> GroupingCandidate {
        GroupingCandidate {
            candidate_id: id.to_string(),
            source_revision: Some("revision-1".to_string()),
            baseline_rank: rank,
            learned_rank: rank,
            features: GroupingFeatureInput {
                exact_source_identity: Some(source.to_string()),
                ..Default::default()
            },
        }
    }

    fn semantic_candidate(
        id: &str,
        source_revision: &str,
        input_revision: i64,
    ) -> GroupingCandidate {
        let semantic = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&serde_json::json!({
                "communication_type": "system_notice",
                "requested_action": "attend",
                "action_owner": "owner",
                "direct_request_probability": 0.8,
                "broadcast_probability": 0.1,
                "personal_obligation_probability": 0.9,
                "information_value_probability": 0.7,
                "deadline": { "kind": "none" },
                "campaign_or_event_identity": "event-1",
                "evidence_refs": ["subject"]
            })),
            input_revision,
            "1.1.0",
            &crate::magician_v2::attention::learning::SemanticExtractorIdentity::default(),
        );
        GroupingCandidate {
            candidate_id: id.to_string(),
            source_revision: Some(source_revision.to_string()),
            baseline_rank: 1,
            learned_rank: 1,
            features: GroupingFeatureInput {
                semantic: Some(semantic),
                ..Default::default()
            },
        }
    }

    #[test]
    fn canonical_pairs_are_unordered_and_revision_bound() {
        let a = AttentionPairCandidateRef {
            candidate_id: "a".to_string(),
            source_revision: Some("r1".to_string()),
        };
        let b = AttentionPairCandidateRef {
            candidate_id: "b".to_string(),
            source_revision: Some("r2".to_string()),
        };
        assert_eq!(
            canonical_pair(a.clone(), b.clone()).unwrap(),
            (a.clone(), b.clone())
        );
        assert_eq!(canonical_pair(b, a.clone()).unwrap().0, a);
    }

    #[test]
    fn owner_cannot_link_blocks_exact_identity_and_transitive_merge() {
        let candidates = vec![
            candidate("a", "source-1", 1),
            candidate("b", "source-1", 2),
            candidate("c", "source-1", 3),
        ];
        let (left, right) =
            canonical_pair(candidates[0].candidate_ref(), candidates[2].candidate_ref()).unwrap();
        let projection = cluster_candidates(
            &snapshot(),
            &candidates,
            &[PersistedPairEvidence {
                left,
                right,
                label: AttentionPairLabelKind::NotDuplicate,
                source: AttentionPairLabelSource::Owner,
                confidence: 1.0,
            }],
        )
        .unwrap();
        assert_eq!(projection.clusters.len(), 2);
        assert!(projection.clusters.iter().all(|cluster| {
            !(cluster.member_ids.contains(&"a".to_string())
                && cluster.member_ids.contains(&"c".to_string()))
        }));
    }

    #[test]
    fn clustering_is_reproducible_and_representative_is_best_ranked_member() {
        let candidates = vec![candidate("b", "source-1", 2), candidate("a", "source-1", 1)];
        let first = cluster_candidates(&snapshot(), &candidates, &[]).unwrap();
        let second = cluster_candidates(&snapshot(), &candidates, &[]).unwrap();
        assert_eq!(first.clusters, second.clusters);
        assert_eq!(first.clusters[0].representative_id, "a");
    }

    #[test]
    fn semantic_pair_features_require_revision_compatible_envelopes() {
        let mut semantic_snapshot = snapshot();
        semantic_snapshot.feature_names = vec!["semantic.same_campaign_event".to_string()];

        let stale_left = semantic_candidate("a", "distill:8", 7);
        let stale_right = semantic_candidate("b", "distill:8", 7);
        assert!(infer_pair(&semantic_snapshot, &stale_left, &stale_right)
            .unwrap()
            .is_none());

        let compatible_left = semantic_candidate("a", "distill:7", 7);
        let compatible_right = semantic_candidate("b", "distill:7", 7);
        assert!(
            infer_pair(&semantic_snapshot, &compatible_left, &compatible_right)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn required_pair_count_is_exact_at_boundaries_and_saturates_on_overflow() {
        assert_eq!(required_pair_evaluations(0), 0);
        assert_eq!(required_pair_evaluations(1), 0);
        assert_eq!(required_pair_evaluations(2), 1);
        assert_eq!(required_pair_evaluations(3), 3);
        assert_eq!(required_pair_evaluations(2_014), 2_027_091);
        assert_eq!(required_pair_evaluations(usize::MAX), u64::MAX);
    }
}
