//! Owner-frozen paired trials and bounded adoption of procedural text. Uses the
//! existing controller and budget ledger; this journal grants no tool authority.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use magician_relentless::{
    budget::{BudgetLedger, OperationClass, OperationIntent},
    contract::{FrozenContract, ProceduralGuidance},
    store::{RunScope, RunStatus},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{LearningCandidateType, LearningScope, LearningStore};
use crate::magician_v2::{
    agents::AgentStorage,
    artifact_v2::workspace::ArtifactV2Workspace,
    jsonl,
    query_analysis::operation_llm_router::OperationLlmRouter,
    relentless_runtime::{PreparedTextDraft, TextDraftProposal},
};

#[cfg(test)]
#[path = "relentless_promotion_tests.rs"]
mod tests;

pub(crate) fn digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidanceEnvelope {
    procedure: ProceduralGuidance,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum CandidateGuidance {
    Direct(ProceduralGuidance),
    Envelope(GuidanceEnvelope),
}
pub(crate) fn candidate_guidance(value: serde_json::Value) -> Result<ProceduralGuidance> {
    Ok(
        match serde_json::from_value::<CandidateGuidance>(value).context(
            "procedural candidate must use the strict guidance schema, directly or under procedure",
        )? {
            CandidateGuidance::Direct(guidance) => guidance,
            CandidateGuidance::Envelope(envelope) => envelope.procedure,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationPhase {
    Evaluation,
    Canary,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceVariant {
    Baseline,
    Candidate,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationMetric {
    WeightedQuality,
    Acceptance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseRequirements {
    pub metric: QualificationMetric,
    pub minimum_mean_gain: f64,
    pub maximum_case_regression: f64,
    pub minimum_candidate_acceptance_rate: f64,
    pub maximum_acceptance_regressions: u32,
}
impl PhaseRequirements {
    fn validate(&self) -> Result<()> {
        ensure!(
            [
                self.minimum_mean_gain,
                self.maximum_case_regression,
                self.minimum_candidate_acceptance_rate
            ]
            .into_iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
            "qualification thresholds must be finite values in [0,1]"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationCase {
    pub id: String,
    pub proposal: TextDraftProposal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuidanceTarget {
    pub task_id: String,
    pub task_context_hash: String,
    pub baseline_contract_hash: String,
    pub minimum_quality: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuidancePromotionSpec {
    pub name: String,
    pub candidate_id: String,
    pub agent_id: String,
    pub ui_thread_id: String,
    pub baseline_guidance: Option<ProceduralGuidance>,
    pub training_run_ids: Vec<Uuid>,
    pub evaluation_cases: Vec<QualificationCase>,
    pub canary_cases: Vec<QualificationCase>,
    pub evaluation_requirements: PhaseRequirements,
    pub canary_requirements: PhaseRequirements,
    pub maximum_evaluation_cost_microusd: u64,
    pub maximum_canary_cost_microusd: u64,
    pub maximum_guidance_bytes: u64,
    pub targets: Vec<GuidanceTarget>,
    pub maximum_adopted_runs: u32,
    pub disable_after_regressions: u32,
    pub unknown_monitor_disables_adoption: bool,
    pub monitor_nonacceptance_is_regression: bool,
    pub expires_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenQualificationCase {
    pub id: String,
    pub baseline: TextDraftProposal,
    pub candidate: TextDraftProposal,
    pub baseline_contract: FrozenContract,
    pub candidate_contract: FrozenContract,
    pub input_hash: String,
}
impl FrozenQualificationCase {
    pub fn proposal(&self, variant: GuidanceVariant) -> &TextDraftProposal {
        match variant {
            GuidanceVariant::Baseline => &self.baseline,
            GuidanceVariant::Candidate => &self.candidate,
        }
    }
    pub fn contract(&self, variant: GuidanceVariant) -> &FrozenContract {
        match variant {
            GuidanceVariant::Baseline => &self.baseline_contract,
            GuidanceVariant::Candidate => &self.candidate_contract,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenGuidancePolicy {
    pub id: Uuid,
    pub scope: RunScope,
    pub version_hash: String,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub spec: GuidancePromotionSpec,
    pub guidance: ProceduralGuidance,
    pub candidate_proposal_hash: String,
    pub evaluation: Vec<FrozenQualificationCase>,
    pub canary: Vec<FrozenQualificationCase>,
    #[serde(default, skip_serializing_if = "QualificationArithmetic::is_legacy")]
    pub arithmetic: QualificationArithmetic,
}

/// The frozen policy owns its arithmetic, including replay of older decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationArithmetic {
    #[default]
    BinaryV1,
    DecimalV2,
}
impl QualificationArithmetic {
    fn is_legacy(&self) -> bool {
        *self == Self::BinaryV1
    }
}

fn qualification_decimal(value: f64) -> Result<Decimal> {
    ensure!(value.is_finite(), "qualification value must be finite");
    // Use the persisted JSON number's shortest round-trip decimal, rather than
    // its binary expansion. No epsilon or quality tolerance changes the gate.
    let decimal = value
        .to_string()
        .parse::<Decimal>()
        .context("qualification value exceeds decimal precision")?;
    ensure!(
        decimal.to_string().parse::<f64>()? == value,
        "qualification value cannot be represented without rounding"
    );
    Ok(decimal)
}

/// Includes the goal and source content, excludes guidance, source aliases and
/// rubric changes. Freshness is an exact-input audit; the owner curates the suite.
pub(crate) fn input_hash(contract: &FrozenContract) -> Result<String> {
    let d = contract.definition();
    let mut contents: Vec<_> = d
        .sources
        .iter()
        .map(|source| &source.content_hash)
        .collect();
    contents.sort();
    digest(&(
        "relentless-held-out-input-v1",
        &d.goal,
        &d.artifact_kind,
        contents,
    ))
}
fn comparison_hash(contract: &FrozenContract) -> Result<String> {
    let d = contract.definition();
    digest(&(
        &d.goal,
        &d.artifact_kind,
        &d.criteria,
        &d.critics,
        &d.sources,
        d.score_at_least,
        &d.review,
        &d.budget,
    ))
}
fn resolved_proposal(
    router: &OperationLlmRouter,
    proposal: &TextDraftProposal,
) -> Result<TextDraftProposal> {
    let prepared = PreparedTextDraft::resolve(router, proposal)?;
    let mut resolved = proposal.clone();
    resolved.native_harness = prepared.setup.native_harness;
    resolved.artifact_review = prepared.setup.artifact_review;
    for criterion in &mut resolved.criteria {
        let pin = prepared
            .contract
            .definition()
            .criteria
            .iter()
            .find(|pin| pin.id == criterion.id)
            .context("resolved criterion pin missing")?;
        criterion.specification = prepared
            .setup
            .specifications
            .0
            .get(&pin.specification_hash)
            .context("resolved criterion missing")?
            .clone();
    }
    Ok(resolved)
}
impl FrozenGuidancePolicy {
    pub fn cases(&self, phase: QualificationPhase) -> &[FrozenQualificationCase] {
        match phase {
            QualificationPhase::Evaluation => &self.evaluation,
            QualificationPhase::Canary => &self.canary,
        }
    }
    pub fn requirements(&self, phase: QualificationPhase) -> &PhaseRequirements {
        match phase {
            QualificationPhase::Evaluation => &self.spec.evaluation_requirements,
            QualificationPhase::Canary => &self.spec.canary_requirements,
        }
    }
    pub fn budget(&self, phase: QualificationPhase) -> u64 {
        match phase {
            QualificationPhase::Evaluation => self.spec.maximum_evaluation_cost_microusd,
            QualificationPhase::Canary => self.spec.maximum_canary_cost_microusd,
        }
    }
    fn content_hash(&self) -> Result<String> {
        digest(&(
            match self.arithmetic {
                QualificationArithmetic::BinaryV1 => "relentless-guidance-policy-v1",
                QualificationArithmetic::DecimalV2 => "relentless-guidance-policy-v2",
            },
            self.id,
            &self.scope,
            &self.approved_by,
            &self.spec,
            &self.guidance,
            &self.candidate_proposal_hash,
            &self.evaluation,
            &self.canary,
        ))
    }
    pub fn validate_binding(&self) -> Result<()> {
        ensure!(
            !self.id.is_nil()
                && self.approved_by == self.scope.principal
                && self.content_hash()? == self.version_hash,
            "guidance policy content/owner binding changed"
        );
        self.guidance.validate()?;
        for case in self.evaluation.iter().chain(&self.canary) {
            ensure!(
                comparison_hash(&case.baseline_contract)?
                    == comparison_hash(&case.candidate_contract)?
                    && input_hash(&case.baseline_contract)? == case.input_hash
                    && case.baseline.actor_guidance == self.spec.baseline_guidance
                    && case.candidate.actor_guidance.as_ref() == Some(&self.guidance),
                "paired trial configuration changed"
            );
            let mut candidate_without_guidance = case.candidate.clone();
            candidate_without_guidance.actor_guidance = case.baseline.actor_guidance.clone();
            ensure!(
                digest(&candidate_without_guidance)? == digest(&case.baseline)?,
                "paired trial changed more than procedural guidance"
            );
        }
        Ok(())
    }
    pub fn resolve(
        workspace: ArtifactV2Workspace,
        router: &OperationLlmRouter,
        scope: RunScope,
        id: Uuid,
        spec: GuidancePromotionSpec,
    ) -> Result<Self> {
        ensure!(
            magicllm::LlmScope::new(&scope.principal, &scope.workspace).is_valid() && !id.is_nil(),
            "invalid guidance policy identity"
        );
        spec.evaluation_requirements.validate()?;
        spec.canary_requirements.validate()?;
        ensure!(
            spec.evaluation_requirements.minimum_mean_gain > 0.0,
            "automatic promotion requires a positive held-out gain floor"
        );
        ensure!(
            !spec.name.trim().is_empty()
                && !spec.agent_id.trim().is_empty()
                && !spec.ui_thread_id.trim().is_empty()
                && !spec.training_run_ids.is_empty()
                && !spec.evaluation_cases.is_empty()
                && !spec.canary_cases.is_empty()
                && !spec.targets.is_empty()
                && spec.maximum_guidance_bytes > 0
                && spec.maximum_evaluation_cost_microusd > 0
                && spec.maximum_canary_cost_microusd > 0
                && spec.maximum_adopted_runs > 0
                && spec.disable_after_regressions > 0
                && spec.expires_at > Utc::now(),
            "guidance policy needs finite phases, targets, bounds and future expiry"
        );
        let candidate = LearningStore::new(workspace).read_candidate(
            &LearningScope::new(&scope.principal, &scope.workspace),
            &spec.candidate_id,
        )?;
        ensure!(
            candidate.candidate_type == LearningCandidateType::MemoryProcedure
                && !candidate.state.is_terminal()
                && candidate
                    .source_agent_id
                    .as_deref()
                    .is_none_or(|agent| agent == spec.agent_id),
            "guidance requires an unpromoted procedural candidate for the admitted agent"
        );
        for evidence in &candidate.evidence_refs {
            if evidence.kind == "relentless_training_run" {
                let id = Uuid::parse_str(
                    evidence
                        .id
                        .as_deref()
                        .context("training provenance lost its run ID")?,
                )?;
                ensure!(spec.training_run_ids.contains(&id),
                    "qualification policy must include every run declared as candidate training provenance");
            }
        }
        let guidance = candidate_guidance(candidate.proposed_change.clone())?;
        guidance.validate()?;
        ensure!(
            serde_json::to_vec(&guidance)?.len() as u64 <= spec.maximum_guidance_bytes,
            "candidate guidance exceeds the owner bound"
        );
        if let Some(baseline) = &spec.baseline_guidance {
            baseline.validate()?;
            ensure!(
                baseline != &guidance,
                "candidate guidance is identical to its frozen baseline"
            );
            ensure!(
                serde_json::to_vec(baseline)?.len() as u64 <= spec.maximum_guidance_bytes,
                "baseline guidance exceeds the owner bound"
            );
        }
        let mut target_ids = BTreeSet::new();
        for target in &spec.targets {
            ensure!(
                !target.task_id.trim().is_empty()
                    && target_ids.insert(&target.task_id)
                    && valid_hash(&target.task_context_hash)
                    && valid_hash(&target.baseline_contract_hash)
                    && target
                        .minimum_quality
                        .is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
                "invalid or duplicate guidance target"
            );
        }
        let mut ids = BTreeSet::new();
        let mut inputs = BTreeSet::new();
        let mut freeze = |cases: &[QualificationCase],
                          phase|
         -> Result<Vec<FrozenQualificationCase>> {
            let mut total = 0u64;
            let mut frozen = Vec::new();
            for case in cases {
                ensure!(
                    !case.id.trim().is_empty()
                        && ids.insert(case.id.clone())
                        && case.proposal.handoff_kind
                            == magician_relentless::routing::HandoffKind::InternalDraft
                        && case
                            .proposal
                            .native_harness
                            .as_ref()
                            .is_none_or(|r| r.agent_id == spec.agent_id),
                    "qualification cases must be unique internal drafts for the policy agent"
                );
                let mut baseline = resolved_proposal(router, &case.proposal)?;
                baseline.actor_guidance = spec.baseline_guidance.clone();
                let mut proposed = baseline.clone();
                proposed.actor_guidance = Some(guidance.clone());
                let baseline_contract = PreparedTextDraft::resolve(router, &baseline)?.contract;
                let candidate_contract = PreparedTextDraft::resolve(router, &proposed)?.contract;
                ensure!(
                    baseline_contract
                        .definition()
                        .critics
                        .iter()
                        .any(|critic| critic.model.is_some()),
                    "guidance qualification requires independent model reviews"
                );
                if matches!(
                    match phase {
                        QualificationPhase::Evaluation => spec.evaluation_requirements.metric,
                        QualificationPhase::Canary => spec.canary_requirements.metric,
                    },
                    QualificationMetric::WeightedQuality
                ) {
                    ensure!(
                        baseline_contract
                            .definition()
                            .criteria
                            .iter()
                            .any(|c| c.weight.is_some()),
                        "weighted quality qualification needs scored criteria"
                    );
                }
                let hash = input_hash(&baseline_contract)?;
                ensure!(
                    inputs.insert(hash.clone()),
                    "evaluation and canary inputs must be distinct held-out cases"
                );
                ensure!(baseline.budget.max_cost_microusd > 0,
                    "automatic guidance qualification requires a finite per-trial cost cap within its owner-approved aggregate budget");
                total = total
                    .checked_add(baseline.budget.max_cost_microusd)
                    .and_then(|v| v.checked_add(proposed.budget.max_cost_microusd))
                    .context("qualification budget overflow")?;
                frozen.push(FrozenQualificationCase {
                    id: case.id.clone(),
                    baseline,
                    candidate: proposed,
                    baseline_contract,
                    candidate_contract,
                    input_hash: hash,
                });
            }
            let ceiling = match phase {
                QualificationPhase::Evaluation => spec.maximum_evaluation_cost_microusd,
                QualificationPhase::Canary => spec.maximum_canary_cost_microusd,
            };
            ensure!(
                total <= ceiling,
                "phase budget must cover all frozen trial ceilings"
            );
            Ok(frozen)
        };
        let evaluation = freeze(&spec.evaluation_cases, QualificationPhase::Evaluation)?;
        let canary = freeze(&spec.canary_cases, QualificationPhase::Canary)?;
        let actor = &evaluation[0].baseline_contract.definition().actor;
        ensure!(
            evaluation.iter().chain(&canary).all(|case| {
                let actual = &case.baseline_contract.definition().actor;
                actual.profile == actor.profile
                    && actual.model == actor.model
                    && actual.family == actor.family
            }),
            "all qualification phases must use the same actor profile/model/family"
        );
        let mut policy = Self {
            id,
            scope: scope.clone(),
            version_hash: String::new(),
            approved_by: scope.principal,
            approved_at: Utc::now(),
            spec,
            guidance,
            candidate_proposal_hash: digest(&candidate.proposed_change)?,
            evaluation,
            canary,
            arithmetic: QualificationArithmetic::DecimalV2,
        };
        policy.version_hash = policy.content_hash()?;
        policy.validate_binding()?;
        Ok(policy)
    }
    pub fn trial_id(
        &self,
        phase: QualificationPhase,
        case_id: &str,
        variant: GuidanceVariant,
    ) -> Result<Uuid> {
        let hash = digest(&(
            "relentless-guidance-trial-v1",
            &self.scope,
            self.id,
            &self.version_hash,
            phase,
            case_id,
            variant,
        ))?;
        let hash = blake3::hash(hash.as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hash.as_bytes()[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(Uuid::from_bytes(bytes))
    }
    pub fn procedure_id(&self) -> String {
        format!("lp_relentless_{}_{}", self.id.simple(), self.version_hash)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuidanceRunOrigin {
    Trial {
        policy_id: Uuid,
        version_hash: String,
        phase: QualificationPhase,
        case_id: String,
        variant: GuidanceVariant,
    },
    Adoption {
        policy_id: Uuid,
        version_hash: String,
        task_id: String,
        baseline_contract_hash: String,
    },
}
impl GuidanceRunOrigin {
    pub fn is_trial(&self) -> bool {
        matches!(self, Self::Trial { .. })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidancePolicyStatus {
    Evaluating,
    Canary,
    Qualified,
    Promoted,
    Rejected,
    Paused,
    Revoked,
    Regressed,
    Expired,
}
impl GuidancePolicyStatus {
    pub fn phase(self) -> Option<QualificationPhase> {
        match self {
            Self::Evaluating => Some(QualificationPhase::Evaluation),
            Self::Canary => Some(QualificationPhase::Canary),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialReservation {
    pub run_id: Uuid,
    pub phase: QualificationPhase,
    pub case_id: String,
    pub variant: GuidanceVariant,
    pub contract_hash: String,
    pub proposal_hash: String,
    pub maximum_cost_microusd: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialObservation {
    pub run_id: Uuid,
    pub contract_hash: String,
    pub source_epoch: u64,
    pub status: RunStatus,
    pub complete: bool,
    pub feasible: bool,
    pub quality: Option<f64>,
    pub accepted: bool,
    pub operator_redirects: u64,
    pub actual_cost_microusd: u64,
    pub evidence_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptionReservation {
    pub run_id: Uuid,
    pub origin: GuidanceRunOrigin,
    pub guided_contract_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptionObservation {
    pub run_id: Uuid,
    pub regressed: bool,
    pub unknown: bool,
    pub evidence_hash: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct GuidancePolicyState {
    pub policy: FrozenGuidancePolicy,
    pub status: GuidancePolicyStatus,
    pub reason: Option<String>,
    pub evaluation_budget: BudgetLedger,
    pub canary_budget: BudgetLedger,
    pub reservations: BTreeMap<Uuid, TrialReservation>,
    pub observations: BTreeMap<Uuid, TrialObservation>,
    pub adoptions: BTreeMap<Uuid, AdoptionReservation>,
    pub monitored: BTreeMap<Uuid, AdoptionObservation>,
    pub procedure_id: Option<String>,
    pub regressions: u32,
    pub paused_from: Option<GuidancePolicyStatus>,
    pub phase_results: BTreeMap<QualificationPhase, (bool, String)>,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Record {
    Activated(FrozenGuidancePolicy),
    Reserved(TrialReservation),
    Observed(TrialObservation),
    PhaseDecided {
        phase: QualificationPhase,
        passed: bool,
        reason: String,
    },
    Promoted {
        procedure_id: String,
        evidence_hash: String,
    },
    Adopted(AdoptionReservation),
    Monitored(AdoptionObservation),
    Paused {
        reason: String,
    },
    Resumed {
        by: String,
    },
    Disabled {
        reason: String,
    },
    Expired {
        at: DateTime<Utc>,
    },
    Revoked {
        by: String,
        at: DateTime<Utc>,
    },
}
impl GuidancePolicyState {
    fn new(policy: FrozenGuidancePolicy) -> Self {
        Self {
            policy,
            status: GuidancePolicyStatus::Evaluating,
            reason: None,
            evaluation_budget: BudgetLedger::default(),
            canary_budget: BudgetLedger::default(),
            reservations: BTreeMap::new(),
            observations: BTreeMap::new(),
            adoptions: BTreeMap::new(),
            monitored: BTreeMap::new(),
            procedure_id: None,
            regressions: 0,
            paused_from: None,
            phase_results: BTreeMap::new(),
        }
    }
    pub fn adoption_enabled(&self) -> bool {
        self.status == GuidancePolicyStatus::Promoted
            && self.policy.spec.expires_at > Utc::now()
            && self.adoptions.len() < self.policy.spec.maximum_adopted_runs as usize
    }
    pub(crate) fn procedure_payload(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({
            "kind":"relentless_qualified_guidance", "explicit_admission_only":true,
            "policy_id":self.policy.id, "policy_version":self.policy.version_hash,
            "guidance_hash":self.policy.guidance.hash()?,
            "qualification_hash":self.qualification_hash()?
        }))
    }
    fn apply(&mut self, record: &Record) -> Result<()> {
        match record {
            Record::Activated(_) => anyhow::bail!("guidance policy cannot be reactivated"),
            Record::Reserved(reservation) => {
                let case = self
                    .policy
                    .cases(reservation.phase)
                    .iter()
                    .find(|c| c.id == reservation.case_id)
                    .context("trial case missing")?;
                ensure!(
                    self.policy.trial_id(
                        reservation.phase,
                        &reservation.case_id,
                        reservation.variant
                    )? == reservation.run_id
                        && case.contract(reservation.variant).hash()? == reservation.contract_hash
                        && digest(case.proposal(reservation.variant))? == reservation.proposal_hash
                        && case.proposal(reservation.variant).budget.max_cost_microusd
                            == reservation.maximum_cost_microusd,
                    "trial reservation differs from frozen case"
                );
                if let Some(old) = self.reservations.get(&reservation.run_id) {
                    ensure!(
                        digest(old)? == digest(reservation)?,
                        "trial reservation rebound"
                    );
                    return Ok(());
                }
                ensure!(
                    self.status.phase() == Some(reservation.phase),
                    "trial reserved outside current phase"
                );
                let ledger = match reservation.phase {
                    QualificationPhase::Evaluation => &mut self.evaluation_budget,
                    QualificationPhase::Canary => &mut self.canary_budget,
                };
                ledger.reserve(
                    OperationIntent {
                        id: reservation.run_id.to_string(),
                        class: OperationClass::Actor,
                        input_hash: reservation.proposal_hash.clone(),
                        maximum_cost_microusd: reservation.maximum_cost_microusd,
                    },
                    self.policy.budget(reservation.phase),
                )?;
                self.reservations
                    .insert(reservation.run_id, reservation.clone());
            },
            Record::Observed(observation) => {
                let reservation = self
                    .reservations
                    .get(&observation.run_id)
                    .context("trial observed before reservation")?;
                ensure!(
                    observation.contract_hash == reservation.contract_hash
                        && observation.source_epoch == 0
                        && observation.status.terminal()
                        && observation.accepted == (observation.status == RunStatus::Accepted)
                        && observation.actual_cost_microusd <= reservation.maximum_cost_microusd
                        && observation
                            .quality
                            .is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v))
                        && (!observation.accepted || observation.complete && observation.feasible)
                        && valid_hash(&observation.evidence_hash),
                    "invalid qualification observation"
                );
                if let Some(old) = self.observations.get(&observation.run_id) {
                    ensure!(
                        digest(old)? == digest(observation)?,
                        "sealed trial observation changed"
                    );
                    return Ok(());
                }
                let ledger = match reservation.phase {
                    QualificationPhase::Evaluation => &mut self.evaluation_budget,
                    QualificationPhase::Canary => &mut self.canary_budget,
                };
                ledger.settle(
                    &observation.run_id.to_string(),
                    observation.actual_cost_microusd,
                    observation.evidence_hash.clone(),
                )?;
                self.observations
                    .insert(observation.run_id, observation.clone());
            },
            Record::PhaseDecided {
                phase,
                passed,
                reason,
            } => {
                if let Some(old) = self.phase_results.get(phase) {
                    ensure!(
                        old == &(*passed, reason.clone()),
                        "sealed phase decision changed"
                    );
                    return Ok(());
                }
                ensure!(
                    self.status.phase() == Some(*phase)
                        && self.assess(*phase)? == Some((*passed, reason.clone())),
                    "qualification decision has no exact complete evidence"
                );
                self.status = if !passed {
                    GuidancePolicyStatus::Rejected
                } else {
                    match phase {
                        QualificationPhase::Evaluation => GuidancePolicyStatus::Canary,
                        QualificationPhase::Canary => GuidancePolicyStatus::Qualified,
                    }
                };
                self.reason = Some(reason.clone());
                self.phase_results.insert(*phase, (*passed, reason.clone()));
            },
            Record::Promoted {
                procedure_id,
                evidence_hash,
            } => {
                if self.procedure_id.as_ref() == Some(procedure_id) {
                    ensure!(
                        procedure_id == &self.policy.procedure_id()
                            && evidence_hash == &self.qualification_hash()?,
                        "promotion replay differs"
                    );
                    return Ok(());
                }
                ensure!(
                    self.status == GuidancePolicyStatus::Qualified
                        && procedure_id == &self.policy.procedure_id()
                        && evidence_hash == &self.qualification_hash()?,
                    "promotion lacks exact canary qualification"
                );
                self.status = GuidancePolicyStatus::Promoted;
                self.procedure_id = Some(procedure_id.clone());
            },
            Record::Adopted(reservation) => {
                if let Some(old) = self.adoptions.get(&reservation.run_id) {
                    ensure!(digest(old)? == digest(reservation)?, "adoption rebound");
                    return Ok(());
                }
                let GuidanceRunOrigin::Adoption {
                    policy_id,
                    version_hash,
                    task_id,
                    baseline_contract_hash,
                } = &reservation.origin
                else {
                    anyhow::bail!("trial cannot become production adoption")
                };
                ensure!(
                    self.status == GuidancePolicyStatus::Promoted
                        && *policy_id == self.policy.id
                        && version_hash == &self.policy.version_hash
                        && self.adoptions.len() < self.policy.spec.maximum_adopted_runs as usize
                        && self
                            .policy
                            .spec
                            .targets
                            .iter()
                            .any(|target| &target.task_id == task_id
                                && &target.baseline_contract_hash == baseline_contract_hash),
                    "adoption is outside approved bounds"
                );
                self.adoptions
                    .insert(reservation.run_id, reservation.clone());
            },
            Record::Monitored(observation) => {
                ensure!(
                    self.adoptions.contains_key(&observation.run_id)
                        && valid_hash(&observation.evidence_hash),
                    "monitoring has no admitted adoption"
                );
                if let Some(old) = self.monitored.get(&observation.run_id) {
                    ensure!(
                        digest(old)? == digest(observation)?,
                        "monitor observation changed"
                    );
                    return Ok(());
                }
                if observation.regressed {
                    self.regressions = self
                        .regressions
                        .checked_add(1)
                        .context("regression count overflow")?;
                }
                if self.status == GuidancePolicyStatus::Promoted
                    && (self.regressions >= self.policy.spec.disable_after_regressions
                        || observation.unknown
                            && self.policy.spec.unknown_monitor_disables_adoption)
                {
                    self.status = GuidancePolicyStatus::Regressed;
                    self.reason = Some(
                        "Declared post-promotion regression/unknown rule disabled future adoption"
                            .into(),
                    );
                }
                self.monitored
                    .insert(observation.run_id, observation.clone());
            },
            Record::Paused { reason } => {
                ensure!(
                    self.status.phase().is_some() || self.status == GuidancePolicyStatus::Qualified,
                    "only qualification may pause"
                );
                self.paused_from = Some(self.status);
                self.status = GuidancePolicyStatus::Paused;
                self.reason = Some(reason.clone());
            },
            Record::Resumed { by } => {
                ensure!(
                    by == &self.policy.approved_by && self.status == GuidancePolicyStatus::Paused,
                    "only the owner may resume a paused frozen qualification"
                );
                let status = self
                    .paused_from
                    .context("paused qualification lost its phase")?;
                ensure!(
                    status.phase().is_some() || status == GuidancePolicyStatus::Qualified,
                    "rejected/revoked/regressed policy cannot resume"
                );
                self.status = status;
                self.paused_from = None;
                self.reason = None;
            },
            Record::Revoked { by, .. } => {
                ensure!(
                    by == &self.policy.approved_by,
                    "only policy owner may revoke"
                );
                self.status = GuidancePolicyStatus::Revoked;
                self.reason = Some("Owner revoked guidance policy".into());
            },
            Record::Disabled { reason } => {
                ensure!(
                    self.status == GuidancePolicyStatus::Promoted,
                    "only promoted guidance may be disabled by projection failure"
                );
                self.status = GuidancePolicyStatus::Regressed;
                self.reason = Some(reason.clone());
            },
            Record::Expired { at } => {
                ensure!(
                    self.status == GuidancePolicyStatus::Promoted
                        && *at >= self.policy.spec.expires_at,
                    "only an expired promoted policy may record expiry"
                );
                self.status = GuidancePolicyStatus::Expired;
                self.reason = Some(
                    "Owner guidance policy expired; future starts use their reviewed baseline"
                        .into(),
                );
            },
        }
        Ok(())
    }
    pub fn qualification_hash(&self) -> Result<String> {
        digest(&(&self.policy.version_hash, &self.observations))
    }
    pub fn assess(&self, phase: QualificationPhase) -> Result<Option<(bool, String)>> {
        let requirements = self.policy.requirements(phase);
        let mut gains = Vec::new();
        let mut decimal_gain = Decimal::ZERO;
        let mut accepted = 0u32;
        let mut regressions = 0u32;
        for case in self.policy.cases(phase) {
            let Some(baseline) = self.observations.get(&self.policy.trial_id(
                phase,
                &case.id,
                GuidanceVariant::Baseline,
            )?) else {
                return Ok(None);
            };
            let Some(candidate) = self.observations.get(&self.policy.trial_id(
                phase,
                &case.id,
                GuidanceVariant::Candidate,
            )?) else {
                return Ok(None);
            };
            if baseline.operator_redirects > 0 || candidate.operator_redirects > 0 {
                return Ok(Some((
                    false,
                    format!(
                        "Case {} received operator assistance and is not a matched frozen trial",
                        case.id
                    ),
                )));
            }
            if !baseline.complete || !candidate.complete || !candidate.feasible {
                return Ok(Some((
                    false,
                    format!(
                        "Case {} lacks complete reviews or candidate must-pass feasibility",
                        case.id
                    ),
                )));
            }
            let scores = match requirements.metric {
                QualificationMetric::Acceptance => Some((
                    if baseline.accepted { 1.0 } else { 0.0 },
                    if candidate.accepted { 1.0 } else { 0.0 },
                )),
                QualificationMetric::WeightedQuality => baseline.quality.zip(candidate.quality),
            };
            let Some((before, after)) = scores else {
                return Ok(Some((
                    false,
                    format!("Case {} has no qualified score", case.id),
                )));
            };
            let gain = after - before;
            let exceeds_regression = match self.policy.arithmetic {
                QualificationArithmetic::BinaryV1 => gain < -requirements.maximum_case_regression,
                QualificationArithmetic::DecimalV2 => {
                    let exact_gain = qualification_decimal(after)? - qualification_decimal(before)?;
                    decimal_gain += exact_gain;
                    exact_gain < -qualification_decimal(requirements.maximum_case_regression)?
                },
            };
            if exceeds_regression {
                return Ok(Some((
                    false,
                    format!("Case {} exceeds the regression bound", case.id),
                )));
            }
            gains.push(gain);
            accepted += u32::from(candidate.accepted);
            regressions += u32::from(baseline.accepted && !candidate.accepted);
        }
        let mean = gains.iter().sum::<f64>() / gains.len() as f64;
        let rate = accepted as f64 / gains.len() as f64;
        let quality_passed = match self.policy.arithmetic {
            QualificationArithmetic::BinaryV1 => {
                mean >= requirements.minimum_mean_gain
                    && rate >= requirements.minimum_candidate_acceptance_rate
            },
            QualificationArithmetic::DecimalV2 => {
                let count = Decimal::from(u64::try_from(gains.len())?);
                decimal_gain >= qualification_decimal(requirements.minimum_mean_gain)? * count
                    && Decimal::from(accepted)
                        >= qualification_decimal(requirements.minimum_candidate_acceptance_rate)?
                            * count
            },
        };
        let passed = quality_passed && regressions <= requirements.maximum_acceptance_regressions;
        Ok(Some((passed, format!("{phase:?}: mean gain {mean:.6}, candidate acceptance {rate:.6}, acceptance regressions {regressions}; policy passed={passed}"))))
    }
}

#[derive(Clone)]
pub struct GuidancePolicyStore {
    workspace: ArtifactV2Workspace,
}
impl GuidancePolicyStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }
    fn path(&self, scope: &RunScope, id: Uuid) -> Result<std::path::PathBuf> {
        ensure!(
            magicllm::LlmScope::new(&scope.principal, &scope.workspace).is_valid(),
            "invalid guidance scope"
        );
        Ok(self
            .workspace
            .scope_root(&scope.principal, &scope.workspace)
            .join("learning")
            .join("relentless_policies")
            .join(format!("{id}.jsonl")))
    }
    fn append(&self, path: &std::path::Path, record: &Record) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        jsonl::append_log_line(&self.workspace, path, &line)
    }
    pub fn load(&self, scope: &RunScope, id: Uuid) -> Result<Option<GuidancePolicyState>> {
        let path = self.path(scope, id)?;
        let Some(raw) = jsonl::read_log_if_present(&self.workspace, &path)? else {
            return Ok(None);
        };
        let mut state = None;
        for record in jsonl::parse_log_lines::<Record>(&raw, &path)? {
            match record {
                Record::Activated(policy) => {
                    ensure!(
                        state.is_none() && policy.id == id && &policy.scope == scope,
                        "guidance activation identity changed"
                    );
                    policy.validate_binding()?;
                    state = Some(GuidancePolicyState::new(policy));
                },
                other => state
                    .as_mut()
                    .context("guidance event precedes activation")?
                    .apply(&other)?,
            }
        }
        Ok(Some(state.context("empty guidance policy journal")?))
    }
    pub fn list(&self, scope: &RunScope) -> Result<Vec<GuidancePolicyState>> {
        self.list_ids(scope)?
            .into_iter()
            .map(|id| {
                self.load(scope, id)?
                    .context("listed guidance policy vanished")
            })
            .collect()
    }

    pub fn list_ids(&self, scope: &RunScope) -> Result<Vec<Uuid>> {
        let directory = self
            .path(scope, Uuid::nil())?
            .parent()
            .context("guidance directory missing")?
            .to_owned();
        jsonl::list_log_paths(&self.workspace, &directory)?
            .into_iter()
            .map(|path| {
                Ok(Uuid::parse_str(
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .context("invalid policy path")?,
                )?)
            })
            .collect()
    }

    /// Serialize derived procedure projection with revocation and all policy
    /// transitions. Callers must project this current state, never their snapshot.
    pub(crate) fn with_current_state<T>(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        project: impl FnOnce(&GuidancePolicyState) -> Result<T>,
    ) -> Result<T> {
        let path = self.path(scope, id)?;
        let _lock = AgentStorage::acquire_file_lock_exclusive_sync(&path)?;
        let state = self.load(scope, id)?.context("guidance policy missing")?;
        ensure!(
            state.policy.version_hash == version,
            "guidance policy version changed"
        );
        project(&state)
    }
    pub fn activate(&self, policy: FrozenGuidancePolicy) -> Result<GuidancePolicyState> {
        policy.validate_binding()?;
        let path = self.path(&policy.scope, policy.id)?;
        // A scope-wide activation lock makes the held-out reuse audit atomic.
        let activation_lock = path
            .parent()
            .context("guidance parent missing")?
            .join("activation.lock");
        let _activation = AgentStorage::acquire_file_lock_exclusive_sync(&activation_lock)?;
        let _lock = AgentStorage::acquire_file_lock_exclusive_sync(&path)?;
        if let Some(old) = self.load(&policy.scope, policy.id)? {
            ensure!(
                old.policy.version_hash == policy.version_hash,
                "policy ID already belongs to a different frozen version"
            );
            return Ok(old);
        }
        ensure!(
            policy.spec.expires_at > Utc::now(),
            "guidance policy expired before activation"
        );
        let inputs: BTreeSet<_> = policy
            .evaluation
            .iter()
            .chain(&policy.canary)
            .map(|c| &c.input_hash)
            .collect();
        for previous in self.list(&policy.scope)? {
            ensure!(
                !previous
                    .policy
                    .evaluation
                    .iter()
                    .chain(&previous.policy.canary)
                    .any(|c| inputs.contains(&c.input_hash)),
                "held-out inputs were already exposed by a prior policy; provide a fresh suite"
            );
            if matches!(
                previous.status,
                GuidancePolicyStatus::Evaluating
                    | GuidancePolicyStatus::Canary
                    | GuidancePolicyStatus::Qualified
                    | GuidancePolicyStatus::Paused
                    | GuidancePolicyStatus::Promoted
            ) && previous.policy.spec.expires_at > Utc::now()
            {
                ensure!(!previous.policy.spec.targets.iter().any(|old| policy.spec.targets.iter()
                    .any(|target| old.task_id == target.task_id)),
                    "another active qualification/adoption policy covers this task; revoke it first");
            }
        }
        self.append(&path, &Record::Activated(policy.clone()))?;
        Ok(GuidancePolicyState::new(policy))
    }
    fn mutate(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        make: impl FnOnce(&GuidancePolicyState) -> Result<Record>,
    ) -> Result<GuidancePolicyState> {
        let path = self.path(scope, id)?;
        let _lock = AgentStorage::acquire_file_lock_exclusive_sync(&path)?;
        let mut state = self.load(scope, id)?.context("guidance policy missing")?;
        ensure!(
            state.policy.version_hash == version,
            "guidance policy version changed"
        );
        let record = make(&state)?;
        state.apply(&record)?;
        self.append(&path, &record)?;
        Ok(state)
    }
    pub(crate) fn reserve(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        reservation: TrialReservation,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |s| {
            ensure!(
                s.policy.spec.expires_at > Utc::now(),
                "qualification policy expired"
            );
            Ok(Record::Reserved(reservation))
        })
    }
    pub(crate) fn observe(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        observation: TrialObservation,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |state| {
            if let Some(old) = state.observations.get(&observation.run_id) {
                // Peer workers may seal the same outcome at different journal
                // generations. Preserve the first proof and its budget receipt;
                // all measured/decision fields must still agree exactly.
                let mut same_result = observation;
                same_result.evidence_hash = old.evidence_hash.clone();
                ensure!(
                    digest(old)? == digest(&same_result)?,
                    "sealed trial result changed"
                );
                Ok(Record::Observed(old.clone()))
            } else {
                Ok(Record::Observed(observation))
            }
        })
    }
    pub(crate) fn decide(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        phase: QualificationPhase,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |s| {
            let (passed, reason) = s
                .assess(phase)?
                .context("qualification evidence incomplete")?;
            Ok(Record::PhaseDecided {
                phase,
                passed,
                reason,
            })
        })
    }
    pub(crate) fn promote(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |s| {
            ensure!(
                s.policy.spec.expires_at > Utc::now(),
                "qualified policy expired"
            );
            Ok(Record::Promoted {
                procedure_id: s.policy.procedure_id(),
                evidence_hash: s.qualification_hash()?,
            })
        })
    }
    pub(crate) fn adopt(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        reservation: AdoptionReservation,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |s| {
            ensure!(
                s.adoption_enabled() || s.adoptions.contains_key(&reservation.run_id),
                "guidance adoption disabled/exhausted"
            );
            Ok(Record::Adopted(reservation))
        })
    }
    pub(crate) fn monitor(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        observation: AdoptionObservation,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |state| {
            if let Some(old) = state.monitored.get(&observation.run_id) {
                let mut same_result = observation;
                same_result.evidence_hash = old.evidence_hash.clone();
                ensure!(
                    digest(old)? == digest(&same_result)?,
                    "sealed monitor result changed"
                );
                Ok(Record::Monitored(old.clone()))
            } else {
                Ok(Record::Monitored(observation))
            }
        })
    }
    pub(crate) fn pause(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        reason: String,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |_| Ok(Record::Paused { reason }))
    }
    pub(crate) fn disable(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
        reason: String,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |_| Ok(Record::Disabled { reason }))
    }
    pub(crate) fn expire(
        &self,
        scope: &RunScope,
        id: Uuid,
        version: &str,
    ) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |_| {
            Ok(Record::Expired { at: Utc::now() })
        })
    }
    pub fn resume(&self, scope: &RunScope, id: Uuid, version: &str) -> Result<GuidancePolicyState> {
        // Activation and resume must serialize target ownership. A pause keeps
        // its frozen target reservation until the owner revokes the policy.
        let path = self.path(scope, id)?;
        let activation_lock = path
            .parent()
            .context("guidance parent missing")?
            .join("activation.lock");
        let _activation = AgentStorage::acquire_file_lock_exclusive_sync(&activation_lock)?;
        self.mutate(scope, id, version, |state| {
            ensure!(
                state.policy.spec.expires_at > Utc::now(),
                "qualification policy expired"
            );
            for previous in self.list(scope)? {
                if previous.policy.id != id
                    && matches!(previous.status,
                        GuidancePolicyStatus::Evaluating | GuidancePolicyStatus::Canary
                            | GuidancePolicyStatus::Qualified | GuidancePolicyStatus::Paused
                            | GuidancePolicyStatus::Promoted)
                    && previous.policy.spec.expires_at > Utc::now()
                {
                    ensure!(!previous.policy.spec.targets.iter().any(|old|
                        state.policy.spec.targets.iter().any(|target| old.task_id == target.task_id)),
                        "another qualification/adoption policy covers this task; revoke it before resuming");
                }
            }
            Ok(Record::Resumed {
                by: scope.principal.clone(),
            })
        })
    }
    pub fn revoke(&self, scope: &RunScope, id: Uuid, version: &str) -> Result<GuidancePolicyState> {
        self.mutate(scope, id, version, |_| {
            Ok(Record::Revoked {
                by: scope.principal.clone(),
                at: Utc::now(),
            })
        })
    }
}
