//! Observe shadow/canary memory-effect records and recommend the next
//! rollout step. Does not change the compiled `MEMORY_EFFECT_MODE` const.
//! An owner Accept on HITL / `/memory` writes a runtime override.

use std::sync::Arc;

use serde::Serialize;
use tracing::warn;

use crate::magician_v2::user_requests::{RequestOption, UserRequest, UserRequestService};

use super::memory_effects::{
    effective_memory_effect_mode, last_memory_effect_prompt, record_memory_effect_prompt,
    set_memory_effect_mode, MemoryEffectMode, MemoryJudgement, MEMORY_EFFECT_MODE,
};
use super::store::ResurfacingStore;

const MIN_JUDGEMENTS_FOR_CANARY: u32 = 20;
const MIN_EXPLAINS_FOR_CANARY: u32 = 3;
const MIN_WOULD_SUPPRESS_FOR_CANARY: u32 = 1;
const MIN_JUDGEMENTS_FOR_ENFORCED: u32 = 50;
const REVIEW_JUDGEMENT_LIMIT: usize = 200;
const HITL_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;
const LOG_TARGET: &str = "resurfacing::memory_effect_review";

pub const REQUEST_TYPE: &str = "memory_effect_review";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEffectAdvice {
    CollectShadowEvidence,
    InvestigateAttachment,
    AdvanceToCanary,
    StayInCanaryCollectLabels,
    AdvanceToEnforced,
    StayEnforced,
}

impl MemoryEffectAdvice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CollectShadowEvidence => "collect_shadow_evidence",
            Self::InvestigateAttachment => "investigate_attachment",
            Self::AdvanceToCanary => "advance_to_canary",
            Self::StayInCanaryCollectLabels => "stay_in_canary_collect_labels",
            Self::AdvanceToEnforced => "advance_to_enforced",
            Self::StayEnforced => "stay_enforced",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryEffectObservation {
    pub mode: MemoryEffectMode,
    pub judgement_count: u32,
    pub would_suppress_count: u32,
    pub explain_count: u32,
    pub conflict_count: u32,
    pub unique_memory_keys: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryEffectReview {
    pub observation: MemoryEffectObservation,
    pub advice: MemoryEffectAdvice,
    pub reason: String,
    pub next_step: String,
}

impl MemoryEffectObservation {
    pub fn from_judgements(mode: MemoryEffectMode, judgements: &[MemoryJudgement]) -> Self {
        let mut unique = std::collections::BTreeSet::<String>::new();
        let mut would_suppress_count = 0;
        let mut explain_count = 0;
        let mut conflict_count = 0;
        for judgement in judgements {
            if judgement.would_suppress {
                would_suppress_count += 1;
            }
            if !judgement.conflicts.is_empty() {
                conflict_count += 1;
            }
            for application in &judgement.applications.would_apply {
                unique.insert(application.memory_key.clone());
                if application.direction == "explain" {
                    explain_count += 1;
                }
            }
        }
        Self {
            mode,
            judgement_count: judgements.len() as u32,
            would_suppress_count,
            explain_count,
            conflict_count,
            unique_memory_keys: unique.len() as u32,
        }
    }
}

pub fn review_live_mode(judgements: &[MemoryJudgement]) -> MemoryEffectReview {
    recommend(MemoryEffectObservation::from_judgements(
        effective_memory_effect_mode(),
        judgements,
    ))
}

pub fn recommend(observation: MemoryEffectObservation) -> MemoryEffectReview {
    let (advice, reason, next_step) = decide(&observation);
    MemoryEffectReview {
        observation,
        advice,
        reason,
        next_step,
    }
}

pub fn target_mode_for_advice(advice: MemoryEffectAdvice) -> Option<MemoryEffectMode> {
    match advice {
        MemoryEffectAdvice::AdvanceToCanary => Some(MemoryEffectMode::Canary),
        MemoryEffectAdvice::AdvanceToEnforced => Some(MemoryEffectMode::Enforced),
        _ => None,
    }
}

pub fn should_emit_hitl(
    advice: MemoryEffectAdvice,
    pending_same_type: bool,
    last_prompted_advice: Option<&str>,
    last_prompted_count: Option<u32>,
    judgement_count: u32,
) -> bool {
    if pending_same_type || target_mode_for_advice(advice).is_none() {
        return false;
    }
    if last_prompted_advice == Some(advice.as_str()) && last_prompted_count == Some(judgement_count)
    {
        return false;
    }
    true
}

pub fn apply_review_decision(
    review: &MemoryEffectReview,
    decision: &str,
) -> Result<MemoryEffectMode, String> {
    match decision {
        "stay" => {
            let _ = record_memory_effect_prompt(
                review.advice.as_str(),
                review.observation.judgement_count,
            );
            Ok(effective_memory_effect_mode())
        },
        "advance" => {
            let Some(target) = target_mode_for_advice(review.advice) else {
                return Err(format!(
                    "advice {} is not an advance",
                    review.advice.as_str()
                ));
            };
            set_memory_effect_mode(target).map_err(|error| error.to_string())?;
            let _ = record_memory_effect_prompt(
                review.advice.as_str(),
                review.observation.judgement_count,
            );
            Ok(target)
        },
        other => Err(format!("unknown decision `{other}`")),
    }
}

pub async fn maybe_prompt_scope(
    store: &ResurfacingStore,
    user_requests: Arc<UserRequestService>,
    principal: &str,
    workspace: &str,
) {
    let judgements = match store.list_recent_memory_judgements_sync(
        principal,
        workspace,
        REVIEW_JUDGEMENT_LIMIT,
    ) {
        Ok(rows) => rows,
        Err(error) => {
            warn!(
                target: LOG_TARGET,
                principal,
                workspace,
                error = %error,
                "memory-effect review skipped; judgement list failed"
            );
            return;
        },
    };
    let review = review_live_mode(&judgements);
    let pending_same_type = user_requests
        .list_pending_for_scope(principal, workspace)
        .await
        .iter()
        .any(|request| request.request_type == REQUEST_TYPE);
    let (last_advice, last_count) = last_memory_effect_prompt();
    if !should_emit_hitl(
        review.advice,
        pending_same_type,
        last_advice.as_deref(),
        last_count,
        review.observation.judgement_count,
    ) {
        return;
    }
    let Some(target) = target_mode_for_advice(review.advice) else {
        return;
    };
    if let Err(error) =
        record_memory_effect_prompt(review.advice.as_str(), review.observation.judgement_count)
    {
        warn!(
            target: LOG_TARGET,
            error = %error,
            "failed to record memory-effect HITL dedupe"
        );
    }
    let request = hitl_request(principal, workspace, &review, target);
    tokio::spawn(async move {
        let response = user_requests.ask(request).await;
        if response.decision == "advance" {
            if let Err(error) = set_memory_effect_mode(target) {
                warn!(
                    target: LOG_TARGET,
                    error = %error,
                    "owner accepted memory-effect advance but persist failed"
                );
            }
        }
    });
}

fn hitl_request(
    principal: &str,
    workspace: &str,
    review: &MemoryEffectReview,
    target: MemoryEffectMode,
) -> UserRequest {
    let stay_label = match review.observation.mode {
        MemoryEffectMode::Shadow => "Keep Shadow",
        MemoryEffectMode::Canary => "Keep Canary",
        MemoryEffectMode::Enforced => "Keep Enforced",
    };
    let advance_label = match target {
        MemoryEffectMode::Canary => "Switch to Canary",
        MemoryEffectMode::Enforced => "Switch to Enforced",
        MemoryEffectMode::Shadow => "Stay in Shadow",
    };
    UserRequest {
        id: String::new(),
        request_type: REQUEST_TYPE.to_owned(),
        question: format!(
            "Memory-effect rollout: {}.\n{}\n{} → {}.",
            review.reason,
            review.next_step,
            review.observation.mode.as_str(),
            target.as_str()
        ),
        options: vec![
            RequestOption {
                id: "advance".to_owned(),
                label: advance_label.to_owned(),
                requires_input: false,
            },
            RequestOption {
                id: "stay".to_owned(),
                label: stay_label.to_owned(),
                requires_input: false,
            },
        ],
        principal: principal.to_owned(),
        workspace: workspace.to_owned(),
        context: serde_json::json!({
            "advice": review.advice.as_str(),
            "compiled_mode": MEMORY_EFFECT_MODE.as_str(),
            "effective_mode": review.observation.mode.as_str(),
            "target_mode": target.as_str(),
            "reason": review.reason,
            "next_step": review.next_step,
            "observation": review.observation,
        }),
        source: "resurfacing".to_owned(),
        execution_id: None,
        task_id: None,
        timeout_secs: HITL_TIMEOUT_SECS,
        default_on_timeout: "stay".to_owned(),
        created_at: 0,
        sensitive: None,
    }
}

fn decide(obs: &MemoryEffectObservation) -> (MemoryEffectAdvice, String, String) {
    if obs.judgement_count == 0 {
        return (
            MemoryEffectAdvice::CollectShadowEvidence,
            "no shadow judgements recorded".to_owned(),
            "confirm a scoped preference, let Today/Worth score, then re-run review_memory_effects"
                .to_owned(),
        );
    }
    if obs.explain_count == 0 && obs.would_suppress_count == 0 {
        return (
            MemoryEffectAdvice::InvestigateAttachment,
            format!(
                "{} judgements recorded but none would explain or suppress",
                obs.judgement_count
            ),
            "check scope on /memory — empty topics/entities never attach".to_owned(),
        );
    }

    match obs.mode {
        MemoryEffectMode::Shadow => {
            if obs.judgement_count >= MIN_JUDGEMENTS_FOR_CANARY
                && obs.explain_count >= MIN_EXPLAINS_FOR_CANARY
                && obs.would_suppress_count >= MIN_WOULD_SUPPRESS_FOR_CANARY
            {
                (
                    MemoryEffectAdvice::AdvanceToCanary,
                    format!(
                        "shadow has {} judgements, {} explains, {} would-hide",
                        obs.judgement_count, obs.explain_count, obs.would_suppress_count
                    ),
                    "accept the AttentionBar prompt or POST /memory/effect-review to switch to Canary (salience only, still no hide)"
                        .to_owned(),
                )
            } else {
                (
                    MemoryEffectAdvice::CollectShadowEvidence,
                    format!(
                        "need ≥{MIN_JUDGEMENTS_FOR_CANARY} judgements, ≥{MIN_EXPLAINS_FOR_CANARY} explains, ≥{MIN_WOULD_SUPPRESS_FOR_CANARY} would-hide; have {}/{}/{}",
                        obs.judgement_count, obs.explain_count, obs.would_suppress_count
                    ),
                    "keep Shadow; confirm more scoped preferences and wait for scorer passes"
                        .to_owned(),
                )
            }
        }
        MemoryEffectMode::Canary => {
            if obs.judgement_count >= MIN_JUDGEMENTS_FOR_ENFORCED
                && obs.would_suppress_count >= MIN_WOULD_SUPPRESS_FOR_CANARY
                && obs.conflict_count * 2 <= obs.would_suppress_count
            {
                (
                    MemoryEffectAdvice::AdvanceToEnforced,
                    format!(
                        "canary has {} judgements, {} would-hide, {} conflicts",
                        obs.judgement_count, obs.would_suppress_count, obs.conflict_count
                    ),
                    "inspect would-hide cards, then accept the AttentionBar prompt or POST /memory/effect-review to switch to Enforced"
                        .to_owned(),
                )
            } else {
                (
                    MemoryEffectAdvice::StayInCanaryCollectLabels,
                    format!(
                        "canary not ready for hide (judgements {}, would-hide {}, conflicts {})",
                        obs.judgement_count, obs.would_suppress_count, obs.conflict_count
                    ),
                    "stay in Canary; collect opens/dismisses on down-ranked cards".to_owned(),
                )
            }
        }
        MemoryEffectMode::Enforced => (
            MemoryEffectAdvice::StayEnforced,
            "enforced hide is already live".to_owned(),
            "watch conflict ratio; POST stay-equivalent is a no-op, or set mode back to Canary if hides look wrong"
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::attention::resurfacing::memory_context::{
        MemoryApplication, MemoryApplicationRecord,
    };
    use crate::magician_v2::attention::resurfacing::memory_effects::{
        reset_memory_effect_runtime_for_test, MEMORY_EFFECT_TEST_LOCK,
    };

    fn judgement(explain: bool, suppress: bool, conflict: bool) -> MemoryJudgement {
        let mut record = MemoryApplicationRecord::default();
        if explain {
            record.would_apply.push(MemoryApplication {
                memory_key: "preferences: vendor".to_owned(),
                memory_revision: Some("1".to_owned()),
                direction: "explain".to_owned(),
                rationale: "you said avoid vendor calls".to_owned(),
                strength: None,
            });
        }
        MemoryJudgement {
            applications: record,
            would_suppress: suppress,
            suppress_reason: suppress.then(|| "stated preference".to_owned()),
            suppress_series_key: None,
            salience_delta: 0.0,
            conflicts: if conflict {
                vec![
                    crate::magician_v2::attention::resurfacing::memory_effects::MemoryConflict {
                        memory_key: "preferences: vendor".to_owned(),
                        rationale: "engagement disagrees".to_owned(),
                        agree_count: 0,
                        disagree_count: 1,
                    },
                ]
            } else {
                Vec::new()
            },
            proposed_action: None,
        }
    }

    #[test]
    fn empty_shadow_stays_collecting() {
        let review = recommend(MemoryEffectObservation::from_judgements(
            MemoryEffectMode::Shadow,
            &[],
        ));
        assert_eq!(review.advice, MemoryEffectAdvice::CollectShadowEvidence);
        assert!(target_mode_for_advice(review.advice).is_none());
    }

    #[test]
    fn judgements_that_never_attach_are_investigated() {
        let rows: Vec<_> = (0..5).map(|_| MemoryJudgement::default()).collect();
        let review = recommend(MemoryEffectObservation::from_judgements(
            MemoryEffectMode::Shadow,
            &rows,
        ));
        assert_eq!(review.advice, MemoryEffectAdvice::InvestigateAttachment);
        assert!(!should_emit_hitl(
            review.advice,
            false,
            None,
            None,
            review.observation.judgement_count
        ));
    }

    #[test]
    fn enough_shadow_evidence_recommends_canary() {
        let mut rows = Vec::new();
        for _ in 0..20 {
            rows.push(judgement(true, true, false));
        }
        let review = recommend(MemoryEffectObservation::from_judgements(
            MemoryEffectMode::Shadow,
            &rows,
        ));
        assert_eq!(review.advice, MemoryEffectAdvice::AdvanceToCanary);
        assert_eq!(
            target_mode_for_advice(review.advice),
            Some(MemoryEffectMode::Canary)
        );
        assert!(review.next_step.contains("AttentionBar"));
    }

    #[test]
    fn shadow_never_jumps_to_enforced() {
        let mut rows = Vec::new();
        for _ in 0..80 {
            rows.push(judgement(true, true, false));
        }
        let review = recommend(MemoryEffectObservation::from_judgements(
            MemoryEffectMode::Shadow,
            &rows,
        ));
        assert_eq!(review.advice, MemoryEffectAdvice::AdvanceToCanary);
        assert_ne!(review.advice, MemoryEffectAdvice::AdvanceToEnforced);
    }

    #[test]
    fn canary_with_thin_labels_stays_put() {
        let review = recommend(MemoryEffectObservation::from_judgements(
            MemoryEffectMode::Canary,
            &[judgement(true, true, false)],
        ));
        assert_eq!(review.advice, MemoryEffectAdvice::StayInCanaryCollectLabels);
    }

    #[test]
    fn hitl_is_deduped_until_the_count_moves() {
        assert!(should_emit_hitl(
            MemoryEffectAdvice::AdvanceToCanary,
            false,
            None,
            None,
            20
        ));
        assert!(!should_emit_hitl(
            MemoryEffectAdvice::AdvanceToCanary,
            true,
            None,
            None,
            20
        ));
        assert!(!should_emit_hitl(
            MemoryEffectAdvice::AdvanceToCanary,
            false,
            Some("advance_to_canary"),
            Some(20),
            20
        ));
        assert!(should_emit_hitl(
            MemoryEffectAdvice::AdvanceToCanary,
            false,
            Some("advance_to_canary"),
            Some(20),
            24
        ));
    }

    #[test]
    fn stay_decision_does_not_change_mode() {
        let _guard = MEMORY_EFFECT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_memory_effect_runtime_for_test();
        let review = recommend(MemoryEffectObservation::from_judgements(
            MemoryEffectMode::Shadow,
            &[],
        ));
        let mode = apply_review_decision(&review, "stay").unwrap();
        assert_eq!(mode, MemoryEffectMode::Shadow);
        assert_eq!(effective_memory_effect_mode(), MemoryEffectMode::Shadow);
        reset_memory_effect_runtime_for_test();
    }

    #[test]
    fn advance_without_ready_advice_is_refused() {
        let _guard = MEMORY_EFFECT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_memory_effect_runtime_for_test();
        let review = recommend(MemoryEffectObservation::from_judgements(
            MemoryEffectMode::Shadow,
            &[],
        ));
        assert!(apply_review_decision(&review, "advance").is_err());
        assert_eq!(effective_memory_effect_mode(), MemoryEffectMode::Shadow);
        reset_memory_effect_runtime_for_test();
    }
}
