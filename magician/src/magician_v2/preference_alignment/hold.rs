//! What a hold is made of, independent of where the act was proposed.
//!
//! The native loop's gate, the plane's `tools/call` and a harness turn all
//! reach a hold from the same [`ActionCheck`]. [`HoldSeed`] is that hold in
//! transportable form — purpose, binding, the intentions it rests on, the
//! judge's offered choices and the owner-private detail — so each path builds
//! the same [`AlignmentAsk`] and the same pause, rather than one path pausing
//! and another refusing.

use serde::{Deserialize, Serialize};

use super::ask::{AlignmentAsk, AskDetail, CoveredRequirement, GENERIC_ASK_TEXT};
use super::contract::ResolutionChoice;
use super::judge::JudgeOutcome;
use super::receipts::AlignmentReceiptPurpose;
use super::service::{ActionCheck, AlignmentAdmission};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldSeed {
    pub purpose: AlignmentReceiptPurpose,
    /// `alignment_binding.v1` of the held act.
    pub binding_id: String,
    pub intention_ids: Vec<String>,
    /// The judge's validated offer (exception holds only).
    #[serde(default)]
    pub judge_choices: Vec<ResolutionChoice>,
    /// Owner-private; never published.
    pub detail: AskDetail,
    /// Other gates this hold's ask also presents (a combined ask). Empty
    /// unless the caller found the act needs another gate's approval too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_covers: Vec<CoveredRequirement>,
}

impl HoldSeed {
    /// The hold a check calls for, or `None` when the act may continue or
    /// must be refused outright. An unresolved check seeds a waiver ask only
    /// when `allow_proceed_without_check` lets the owner waive it.
    pub fn from_check(
        check: &ActionCheck,
        act_summary: &str,
        allow_proceed_without_check: bool,
    ) -> Option<Self> {
        let binding_id = check.binding_id.clone()?;
        match &check.admission {
            AlignmentAdmission::Hold { outcome } => {
                let (explanation, intention_ids, judge_choices, remember_offer) = match outcome {
                    // What the hold rests on is what the owner reviews (and
                    // may change); the rest of what the judge cited is
                    // context for the explanation.
                    JudgeOutcome::Decided { decision, .. } => (
                        decision.decision.explanation.clone(),
                        if decision.decision.holding_intentions.is_empty() {
                            decision.decision.supporting_intentions.clone()
                        } else {
                            decision.decision.holding_intentions.clone()
                        },
                        decision.decision.resolution_choices.clone(),
                        decision.decision.remember_offer.clone(),
                    ),
                    _ => (String::new(), Vec::new(), Vec::new(), None),
                };
                Some(Self {
                    purpose: AlignmentReceiptPurpose::Exception,
                    binding_id,
                    detail: AskDetail {
                        explanation,
                        act_summary: act_summary.to_string(),
                        intentions: check
                            .cited
                            .iter()
                            .filter(|cited| intention_ids.contains(&cited.id))
                            .cloned()
                            .collect(),
                        remember: remember_offer.and_then(|offer| {
                            check.cited.iter().find(|cited| cited.id == offer).cloned()
                        }),
                    },
                    intention_ids,
                    judge_choices,
                    also_covers: Vec::new(),
                })
            },
            AlignmentAdmission::Unresolved { cause } if allow_proceed_without_check => Some(Self {
                purpose: AlignmentReceiptPurpose::CheckWaiver,
                binding_id,
                intention_ids: Vec::new(),
                judge_choices: Vec::new(),
                detail: AskDetail {
                    explanation: format!(
                        "This action could not be checked against your saved preferences \
                         ({cause:?})."
                    ),
                    act_summary: act_summary.to_string(),
                    ..AskDetail::default()
                },
                also_covers: Vec::new(),
            }),
            _ => None,
        }
    }

    /// This hold, presented together with another gate's requirements.
    pub fn covering(mut self, covers: Vec<CoveredRequirement>) -> Self {
        self.also_covers = covers;
        self
    }

    /// The typed ask for this hold. Its `ask_id` is set from the pause when
    /// the pause is built.
    pub fn ask(
        &self,
        execution_id: Option<String>,
        now_ms: i64,
        deadline_secs: u64,
        background: bool,
    ) -> AlignmentAsk {
        let ask = match self.purpose {
            AlignmentReceiptPurpose::Exception => AlignmentAsk::for_hold(
                String::new(),
                self.binding_id.clone(),
                execution_id,
                self.intention_ids.clone(),
                &self.judge_choices,
                now_ms,
                deadline_secs,
                background,
                Some(self.detail.clone()),
            ),
            AlignmentReceiptPurpose::CheckWaiver => {
                let mut ask = AlignmentAsk::for_unresolved_check(
                    String::new(),
                    self.binding_id.clone(),
                    execution_id,
                    Vec::new(),
                    now_ms,
                    deadline_secs,
                    background,
                );
                ask.detail = Some(self.detail.clone());
                ask
            },
        };
        ask.with_also_covers(self.also_covers.clone())
    }

    /// The pause's summary. Neutral: an approval-rule fan-out may carry it.
    pub fn summary(&self, action_type: &str) -> String {
        if !self.also_covers.is_empty() {
            return format!("Review and approve before it runs: {action_type}");
        }
        match self.purpose {
            AlignmentReceiptPurpose::Exception => format!("Review before it runs: {action_type}"),
            AlignmentReceiptPurpose::CheckWaiver => {
                format!("Preference check unavailable: {action_type}")
            },
        }
    }

    /// The pause's reason. A confirmation's reason travels beyond the pause
    /// record (execution summaries, chat hints, a parent's failure text), so
    /// it is the generic prompt; why the act was held stays in the ask's
    /// owner-private detail.
    pub fn reason(&self) -> String {
        if self.also_covers.is_empty() {
            GENERIC_ASK_TEXT.to_string()
        } else {
            format!("{GENERIC_ASK_TEXT} Allowing it also gives the approval it needs.")
        }
    }
}

/// An act the backstop would not let run: a hold the caller may turn into
/// the owner's review (`seed`), or a plain refusal. Carried as the action's
/// error so every caller sees a failure, never a result that reads as
/// success; plane and harness paths downcast it to pause instead.
#[derive(Debug, Clone)]
pub struct AlignmentHeld {
    pub seed: Option<HoldSeed>,
    pub message: String,
}

impl std::fmt::Display for AlignmentHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AlignmentHeld {}
