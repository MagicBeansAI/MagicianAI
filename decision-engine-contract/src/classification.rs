//! Engine-owned admission and qualification policy for classification only.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchStrategy {
    #[default]
    PerItem,
    SharedChunk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClassificationLimits {
    pub max_items: usize,
    pub chunk_size: usize,
    pub item_concurrency: usize,
    pub max_context_bytes: usize,
    pub max_item_bytes: usize,
    pub max_request_bytes: usize,
    pub shadow_budget_ms: u64,
    pub decision_budget_ms: u64,
    /// Extra admission time for background gates. Zero keeps foreground timing.
    pub queue_budget_ms: u64,
}
impl Default for ClassificationLimits {
    fn default() -> Self {
        Self {
            max_items: 64,
            chunk_size: 12,
            item_concurrency: 6,
            max_context_bytes: 8192,
            max_item_bytes: 2048,
            max_request_bytes: 65536,
            shadow_budget_ms: 3000,
            decision_budget_ms: 1000,
            queue_budget_ms: 0,
        }
    }
}
impl ClassificationLimits {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value, ceiling) in [
            ("max_items", self.max_items, 256),
            ("chunk_size", self.chunk_size, self.max_items),
            ("item_concurrency", self.item_concurrency, 32),
            ("max_context_bytes", self.max_context_bytes, 32768),
            ("max_item_bytes", self.max_item_bytes, 16384),
            ("max_request_bytes", self.max_request_bytes, 1048576),
        ] {
            if value == 0 || value > ceiling {
                return Err(format!("{name} must be in 1..={ceiling}"));
            }
        }
        if self.max_context_bytes > self.max_request_bytes
            || self.max_item_bytes > self.max_request_bytes
        {
            return Err("context/item limit exceeds request limit".into());
        }
        if self.shadow_budget_ms == 0
            || self.shadow_budget_ms > 30000
            || self.decision_budget_ms == 0
            || self.decision_budget_ms
                > if self.queue_budget_ms > 0 {
                    120000
                } else {
                    10000
                }
            || self.queue_budget_ms > 120000
        {
            return Err("classification budget exceeds hard bounds".into());
        }
        Ok(())
    }
}

/// Observation-only LLM comparison. It never grants mutation authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClassificationObservationPolicy {
    pub gate_sample_rate: f64,
    pub queue_budget_ms: u64,
    pub inference_budget_ms: u64,
    pub max_pending: usize,
    pub max_per_scope: usize,
    pub max_snapshot_bytes: usize,
    pub max_retained_bytes: usize,
    pub max_per_hour: usize,
    pub max_scope_per_hour: usize,
    /// Conservative reservation ceiling across selected reference calls.
    pub max_reserved_microusd_per_hour: u64,
}

impl Default for ClassificationObservationPolicy {
    fn default() -> Self {
        Self {
            gate_sample_rate: 0.0,
            queue_budget_ms: 30_000,
            inference_budget_ms: 30_000,
            max_pending: 16,
            max_per_scope: 4,
            max_snapshot_bytes: 256 * 1024,
            max_retained_bytes: 4 * 1024 * 1024,
            max_per_hour: 60,
            max_scope_per_hour: 10,
            max_reserved_microusd_per_hour: 10_000_000,
        }
    }
}

impl ClassificationObservationPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !self.gate_sample_rate.is_finite() || !(0.0..=1.0).contains(&self.gate_sample_rate) {
            return Err("gate_sample_rate must be a finite probability".into());
        }
        if self.queue_budget_ms == 0
            || self.queue_budget_ms > 30_000
            || self.inference_budget_ms == 0
            || self.inference_budget_ms > 30_000
        {
            return Err("observation queue/inference budgets must be in 1..=30000ms".into());
        }
        for (name, value, max) in [
            ("max_pending", self.max_pending, 16),
            ("max_per_scope", self.max_per_scope, 4),
            ("max_snapshot_bytes", self.max_snapshot_bytes, 256 * 1024),
            (
                "max_retained_bytes",
                self.max_retained_bytes,
                4 * 1024 * 1024,
            ),
            ("max_per_hour", self.max_per_hour, 60),
            ("max_scope_per_hour", self.max_scope_per_hour, 10),
        ] {
            if value == 0 || value > max {
                return Err(format!("{name} must be in 1..={max}"));
            }
        }
        if self.max_per_scope > self.max_pending
            || self.max_scope_per_hour > self.max_per_hour
            || self.max_snapshot_bytes > self.max_retained_bytes
        {
            return Err("observation limits are internally inconsistent".into());
        }
        if self.max_reserved_microusd_per_hour == 0
            || self.max_reserved_microusd_per_hour > 10_000_000
        {
            return Err("observation spending limit must be in 1..=10000000 microusd".into());
        }
        Ok(())
    }
}

/// An explicitly reviewed output. Enabling a gate does not qualify any label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationQualification {
    pub question: String,
    /// Choice label, `true`/`false` for Noul, or `score` with a reviewed range.
    pub output: String,
    pub model: String,
    pub provider: String,
    pub pack_version: String,
    pub threshold_fingerprint: String,
    pub projection_version: String,
    pub reference_version: String,
    pub evidence_id: String,
    pub human_review_ref: String,
    pub behavior_fingerprint: String,
    #[serde(default)]
    pub score_range: Option<[f64; 2]>,
    #[serde(default)]
    pub consumer_mapping_version: Option<String>,
}
impl ClassificationQualification {
    pub fn validate(&self) -> Result<(), String> {
        if [
            &self.question,
            &self.output,
            &self.model,
            &self.provider,
            &self.pack_version,
            &self.threshold_fingerprint,
            &self.projection_version,
            &self.reference_version,
            &self.evidence_id,
            &self.human_review_ref,
            &self.behavior_fingerprint,
        ]
        .iter()
        .any(|value| value.trim().is_empty() || value.len() > 512)
        {
            return Err(
                "qualification identity and review references must be nonempty and bounded".into(),
            );
        }
        if self.output == "score"
            && (self.score_range.is_none()
                || self
                    .consumer_mapping_version
                    .as_ref()
                    .is_none_or(|v| v.trim().is_empty()))
        {
            return Err("score qualification requires range and consumer mapping version".into());
        }
        if self
            .score_range
            .is_some_and(|[lo, hi]| !lo.is_finite() || !hi.is_finite() || lo > hi)
        {
            return Err("invalid qualification score range".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClassificationPolicy {
    #[serde(default)]
    pub allow_unqualified_gate: bool,
    pub limits: ClassificationLimits,
    pub qualifications: Vec<ClassificationQualification>,
    /// Pack question ID to outputs that require reviewed qualification even
    /// when the operator has enabled unqualified gating.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub restricted_outputs: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub observation: ClassificationObservationPolicy,
    #[serde(default)]
    pub observation_revision: String,
    #[serde(default)]
    pub batch_strategy: BatchStrategy,
    #[serde(default)]
    pub shared_chunk_transform_version: Option<u32>,
    /// Behavior excludes rollout knobs and evidence so approval cannot invalidate itself.
    pub behavior_fingerprint: String,
    pub pack: String,
    pub pack_version: String,
}

/// Content-free provenance retained by a consuming verdict/cache entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationOrigin {
    pub model: crate::identity::ModelIdentity,
    pub pack: String,
    pub pack_version: String,
    pub policy_revision: String,
    pub projection_version: String,
    pub reference_version: String,
    pub batch_id: String,
    pub call_ids: Vec<String>,
    pub qualifications: std::collections::BTreeMap<String, String>,
}
