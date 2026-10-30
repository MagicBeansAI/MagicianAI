use std::collections::BTreeSet;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionKind {
    Check,
    Rubric,
    Grounding,
    Perceptual,
}

/// Hash of the resolved rubric/check/sampling specification, not an alias.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub id: String,
    pub kind: CriterionKind,
    pub specification_hash: String,
    pub weight: Option<f64>,
    pub must_pass: bool,
    pub pass_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPin {
    pub profile: String,
    pub model: String,
    pub family: String,
    pub configuration_hash: String,
}

/// Procedural text only. Capability, budget, policy and acceptance fields are
/// deliberately absent; guidance is actor context and never factual evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProceduralGuidance {
    pub title: String,
    pub workflow: Vec<String>,
    pub decision_points: Vec<String>,
    pub verification: Vec<String>,
    pub failure_modes: Vec<String>,
}

impl ProceduralGuidance {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty()
                && !self.workflow.is_empty()
                && self
                    .workflow
                    .iter()
                    .chain(&self.decision_points)
                    .chain(&self.verification)
                    .chain(&self.failure_modes)
                    .all(|text| !text.trim().is_empty()),
            "procedural guidance needs a title and nonempty workflow text"
        );
        Ok(())
    }
    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
    pub fn bind_actor_pin(&self, mut pin: ModelPin) -> Result<ModelPin> {
        pin.configuration_hash = blake3::hash(&serde_json::to_vec(&(
            "relentless-procedural-guidance-v1",
            &pin.configuration_hash,
            self.hash()?,
        ))?)
        .to_hex()
        .to_string();
        Ok(pin)
    }
}

pub fn bind_guidance_pin(pin: ModelPin, guidance: Option<&ProceduralGuidance>) -> Result<ModelPin> {
    match guidance {
        Some(guidance) => guidance.bind_actor_pin(pin),
        None => Ok(pin),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriticPin {
    pub id: String,
    pub criteria: Vec<String>,
    pub model: Option<ModelPin>,
    pub configuration_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPolicy {
    pub panel_size: u32,
    pub score_quorum: u32,
    pub min_confidence: f64,
    pub max_score_spread: f64,
    pub noise_margin: f64,
    pub max_retries_per_critic: u32,
    #[serde(default, skip_serializing_if = "UnknownReviewPolicy::is_pause")]
    pub unknown_review: UnknownReviewPolicy,
}

/// Revision and acceptance are separate decisions. A qualified defect may
/// permit another bounded draft without turning a disputed review into a pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReviewPolicy {
    #[default]
    Pause,
    ReviseSupportedDefects,
}

impl UnknownReviewPolicy {
    fn is_pause(&self) -> bool {
        *self == Self::Pause
    }
}

/// Integer microdollars avoid floating-point budget overshoot. Each omitted or
/// zero limit is unlimited; positive values are owner-reviewed ceilings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    pub max_attempts: u32,
    pub max_wall_clock_secs: u64,
    pub max_cost_microusd: u64,
    pub plateau_attempts: u32,
}

impl Budget {
    /// Internal arithmetic bound; keep zero in the frozen owner-facing contract.
    pub fn cost_ceiling(&self) -> u64 {
        if self.max_cost_microusd == 0 {
            u64::MAX
        } else {
            self.max_cost_microusd
        }
    }

    pub fn attempt_ceiling(&self) -> u32 {
        if self.max_attempts == 0 {
            u32::MAX
        } else {
            self.max_attempts
        }
    }

    pub fn deadline_at(
        &self,
        start: chrono::DateTime<chrono::Utc>,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        if self.max_wall_clock_secs == 0 {
            return Ok(chrono::DateTime::<chrono::Utc>::MAX_UTC);
        }
        let duration = chrono::Duration::try_seconds(i64::try_from(self.max_wall_clock_secs)?)
            .ok_or_else(|| anyhow::anyhow!("wall clock budget overflow"))?;
        start
            .checked_add_signed(duration)
            .ok_or_else(|| anyhow::anyhow!("deadline overflow"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePin {
    pub id: String,
    pub content_hash: String,
}

/// Bound on evidence collected by a trusted actor adapter. These sources are
/// sealed with each candidate; they never replace owner-pinned evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectedSourceLimits {
    pub maximum_sources: u32,
    pub maximum_bytes: u64,
}

impl CollectedSourceLimits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.maximum_sources > 0 && self.maximum_bytes > 0 && self.maximum_bytes < u64::MAX,
            "collected evidence requires finite count and byte limits"
        );
        Ok(())
    }
}

/// Resolved proposal. Only an authenticated owner/program admission may freeze
/// it; constructing this value is not authorization to dispatch an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionOfDone {
    pub goal: String,
    pub artifact_kind: String,
    pub actor: ModelPin,
    pub criteria: Vec<Criterion>,
    pub critics: Vec<CriticPin>,
    pub sources: Vec<SourcePin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected_source_limits: Option<CollectedSourceLimits>,
    pub score_at_least: Option<f64>,
    pub review: ReviewPolicy,
    #[serde(default)]
    pub budget: Budget,
}

/// Deserialization validates too: a recovered contract cannot bypass admission
/// invariants. No mutable accessor is offered to an actor or reviewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DefinitionOfDone", into = "DefinitionOfDone")]
pub struct FrozenContract(DefinitionOfDone);

impl TryFrom<DefinitionOfDone> for FrozenContract {
    type Error = anyhow::Error;

    fn try_from(value: DefinitionOfDone) -> Result<Self> {
        value.validate()?;
        Ok(Self(value))
    }
}

impl From<FrozenContract> for DefinitionOfDone {
    fn from(value: FrozenContract) -> Self {
        value.0
    }
}

impl FrozenContract {
    pub fn definition(&self) -> &DefinitionOfDone {
        &self.0
    }

    pub fn hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(&self.0)?)
            .to_hex()
            .to_string())
    }
}

/// Whether a pin is a canonical lowercase 256-bit content digest.
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub(super) fn unit(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

impl ModelPin {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(!self.profile.trim().is_empty(), "empty model profile");
        ensure!(!self.model.trim().is_empty(), "empty model pin");
        ensure!(!self.family.trim().is_empty(), "empty model family");
        ensure!(
            valid_hash(&self.configuration_hash),
            "invalid model configuration hash"
        );
        Ok(())
    }
}

impl DefinitionOfDone {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.goal.trim().is_empty(), "empty goal");
        if let Some(limits) = &self.collected_source_limits {
            limits.validate()?;
        }
        ensure!(!self.artifact_kind.trim().is_empty(), "empty artifact kind");
        self.actor.validate()?;
        ensure!(!self.criteria.is_empty(), "empty definition of done");
        let mut ids = BTreeSet::new();
        let mut scored = false;
        let mut total_weight = 0.0;
        for criterion in &self.criteria {
            ensure!(
                !criterion.id.trim().is_empty() && ids.insert(&criterion.id),
                "empty/duplicate criterion ID"
            );
            ensure!(
                valid_hash(&criterion.specification_hash),
                "unresolved criterion specification"
            );
            ensure!(
                criterion.must_pass || criterion.weight.is_some(),
                "criterion does not participate in acceptance"
            );
            if let Some(weight) = criterion.weight {
                ensure!(
                    weight.is_finite() && weight > 0.0,
                    "weight must be finite and positive"
                );
                total_weight += weight;
                scored = true;
            }
            if criterion.kind == CriterionKind::Check {
                ensure!(
                    criterion.must_pass
                        && criterion.weight.is_none()
                        && criterion.pass_score.is_none(),
                    "checks are unscored must-pass assertions"
                );
            } else if criterion.must_pass {
                ensure!(
                    criterion.pass_score.is_some_and(unit),
                    "model must-pass criterion needs a pass threshold"
                );
            } else {
                ensure!(
                    criterion.pass_score.is_none(),
                    "pass threshold on a non-must-pass criterion"
                );
            }
        }
        ensure!(total_weight.is_finite(), "weight sum overflow");
        ensure!(
            if scored {
                self.score_at_least.is_some_and(unit)
            } else {
                self.score_at_least.is_none()
            },
            "scored contracts need a threshold; check-only contracts omit it"
        );
        let review = &self.review;
        ensure!(
            review.panel_size > 0
                && review.score_quorum > 0
                && review.score_quorum <= review.panel_size,
            "invalid review quorum"
        );
        ensure!(
            unit(review.min_confidence)
                && unit(review.max_score_spread)
                && unit(review.noise_margin),
            "invalid review thresholds"
        );
        let budget = &self.budget;
        ensure!(
            chrono::Duration::try_seconds(i64::try_from(budget.max_wall_clock_secs)?).is_some(),
            "wall clock budget overflow"
        );
        let mut critics = BTreeSet::new();
        for critic in &self.critics {
            ensure!(
                !critic.id.trim().is_empty() && critics.insert(&critic.id),
                "empty/duplicate critic ID"
            );
            ensure!(
                valid_hash(&critic.configuration_hash),
                "unresolved critic configuration"
            );
            ensure!(!critic.criteria.is_empty(), "critic has no assignments");
            let assigned: BTreeSet<_> = critic.criteria.iter().collect();
            ensure!(
                assigned.len() == critic.criteria.len()
                    && assigned.iter().all(|id| ids.contains(id)),
                "unknown/duplicate critic assignment"
            );
            if let Some(model) = &critic.model {
                model.validate()?;
                ensure!(
                    !model
                        .family
                        .trim()
                        .eq_ignore_ascii_case(self.actor.family.trim()),
                    "critic is in actor model family"
                );
            }
            for id in &critic.criteria {
                let criterion = self
                    .criteria
                    .iter()
                    .find(|c| &c.id == id)
                    .expect("validated assignment");
                ensure!(
                    (criterion.kind == CriterionKind::Check) == critic.model.is_none(),
                    "critic adapter does not match criterion kind"
                );
            }
        }
        for criterion in &self.criteria {
            let count = self
                .critics
                .iter()
                .filter(|c| c.criteria.contains(&criterion.id))
                .count();
            let expected = if criterion.kind == CriterionKind::Check {
                1
            } else {
                review.panel_size as usize
            };
            ensure!(
                count == expected,
                "criterion {} does not have its required panel",
                criterion.id
            );
        }
        let mut sources = BTreeSet::new();
        for source in &self.sources {
            ensure!(
                !source.id.trim().is_empty()
                    && sources.insert(&source.id)
                    && valid_hash(&source.content_hash),
                "invalid/duplicate source snapshot"
            );
        }
        Ok(())
    }
}
