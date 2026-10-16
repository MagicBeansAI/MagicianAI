//! `alignment_input.v1` and `alignment_decision.v2`, plus the host validator
//! that stands between the judge's answer and anything that acts on it.
//!
//! v2 adds `holding_intentions`: the cited intentions a wait rests on. v1
//! had one list, so a hold could keep full force by citing any explicit
//! intention beside the inferred one it was really about. The force cap now
//! reads only what the hold rests on.
//!
//! The validator checks structure and references, never semantics: valid JSON
//! with real IDs does not make a judgment correct (plan §1.2). What it does
//! guarantee is that a malformed or ungrounded answer never becomes `proceed`
//! — it is rejected, and the caller treats the check as unresolved — and that
//! the force of an intervention never exceeds what the cited evidence may
//! carry (P0 contracts §6.2).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::intention::{Coverage, Intention, InterventionPolicy, StandingGeneration};

pub const ALIGNMENT_INPUT_SCHEMA_VERSION: &str = "alignment_input.v1";
pub const ALIGNMENT_DECISION_SCHEMA_VERSION: &str = "alignment_decision.v2";

/// Longest explanation the host accepts. An answer past it is malformed, not
/// truncated: a cut explanation can drop the reason the owner needs.
pub const MAX_EXPLANATION_CHARS: usize = 1200;

/// One span of the current authenticated owner turn, with a host-issued ID.
/// `cancel` and `acknowledged_exception` must cite one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerSpan {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignmentRequest {
    /// Steer counter: any new owner instruction bumps it and invalidates
    /// captured assessments.
    pub revision: u64,
    pub text: String,
    #[serde(default)]
    pub owner_spans: Vec<OwnerSpan>,
}

/// Facts about the proposed act. `verified` fields come from trusted lowering
/// and catalog owners; `model_interpretations` are the model's reading and
/// stay labelled as such.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionFacts {
    /// `alignment_binding.v1` of the act (P0 contracts §6.4).
    pub binding_id: String,
    pub capability: String,
    pub action: String,
    /// Canonical, source-policy-redacted arguments.
    pub normalized_args: Value,
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outward_class: Option<String>,
    pub consequence_class: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub model_interpretations: Value,
}

/// A recorded owner resolution the host already matched to this scope, act,
/// source revisions and expiry. Typed input, not an invitation for the model
/// to search history for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostMatchedException {
    pub receipt_id: String,
    pub intention_ids: Vec<String>,
    pub scope_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    /// The owner's global choice; `None` means the reviewed defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<InterventionPolicy>,
    pub version: String,
}

impl EffectivePolicy {
    /// The policy in force for one intention: its own refinement first.
    pub fn for_intention(&self, intention: &Intention) -> Option<InterventionPolicy> {
        intention.intervention_policy.or(self.global)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlignmentInput {
    pub schema_version: String,
    pub principal: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub request: AlignmentRequest,
    pub intentions: Vec<Intention>,
    pub coverage: Coverage,
    #[serde(default)]
    pub host_matched_exceptions: Vec<HostMatchedException>,
    /// `None` for a request/planning-time check with no concrete act yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionFacts>,
    /// Other gates still pending for this act. Listed so the judge does not
    /// assume they are resolved; alignment never resolves them.
    #[serde(default)]
    pub pending_gates: Vec<String>,
    pub policy: EffectivePolicy,
    pub generation: StandingGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    Aligned,
    Conflict,
    Unclear,
    AcknowledgedException,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intervention {
    None,
    Nudge,
    Clarify,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Continuation {
    Proceed,
    Wait,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionChoice {
    UseAlternative,
    AllowOnce,
    ChangePreference,
    CorrectMemory,
    Cancel,
}

/// The judge's answer as it arrives. Nothing here is trusted until
/// [`validate_decision`] accepts it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignmentDecision {
    pub schema_version: String,
    pub assessment: Assessment,
    pub intervention: Intervention,
    pub continuation: Continuation,
    #[serde(default)]
    pub supporting_intentions: Vec<String>,
    /// The cited intentions a wait rests on: those whose conflict is why the
    /// act should not continue now. A subset of `supporting_intentions`,
    /// non-empty exactly when `continuation` is `wait`. The force cap reads
    /// only these, so citing an unrelated explicit intention never lends an
    /// inferred conflict the force to hold.
    #[serde(default)]
    pub holding_intentions: Vec<String>,
    /// Host span IDs from the current owner turn. Required for `cancel` and
    /// `acknowledged_exception`.
    #[serde(default)]
    pub cited_owner_spans: Vec<String>,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub unresolved_facts: Vec<String>,
    #[serde(default)]
    pub resolution_choices: Vec<ResolutionChoice>,
    /// Offer to remember an inferred intention as an explicit preference.
    /// Accepting goes through the exact-wording write confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_offer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecisionRejection {
    #[error("unsupported decision schema version `{0}`")]
    SchemaVersion(String),
    #[error("decision cites intention `{0}`, which was not in the input")]
    UnknownIntention(String),
    #[error("decision cites intention `{0}` more than once")]
    DuplicateIntention(String),
    #[error("decision cites owner span `{0}`, which is not in the current owner turn")]
    UnknownSpan(String),
    #[error("contradictory decision: {0}")]
    Contradiction(&'static str),
    #[error("intervention `{0:?}` cites no supporting intention")]
    Ungrounded(Intervention),
    #[error("`{0}` requires a cited span of the current owner turn")]
    MissingOwnerSpan(&'static str),
    #[error("an intervention needs an explanation")]
    MissingExplanation,
    #[error("explanation exceeds {MAX_EXPLANATION_CHARS} characters")]
    ExplanationTooLong,
    #[error("remember_offer `{0}` must name a cited inferred intention")]
    InvalidRememberOffer(String),
    #[error("holding intention `{0}` is not among the supporting intentions")]
    HoldingNotCited(String),
}

/// Recorded when the host lowered the force of an answer to what its cited
/// evidence may carry. The lowering is not a rejection: the semantic judgment
/// stands, only its strength changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceDowngrade {
    pub from_intervention: Intervention,
    pub from_continuation: Continuation,
    pub reason: String,
    /// What the lowered wait rested on (its `holding_intentions`), kept
    /// because lowering clears that list: the nudge is about these, not
    /// everything the answer cited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rested_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedDecision {
    pub decision: AlignmentDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgrade: Option<ForceDowngrade>,
}

/// Validate a judge answer against its input. On `Err` the check is
/// unresolved — the caller holds an act that requires enforcement and never
/// repairs the answer into a permissive one.
pub fn validate_decision(
    input: &AlignmentInput,
    decision: AlignmentDecision,
) -> Result<ValidatedDecision, DecisionRejection> {
    if decision.schema_version != ALIGNMENT_DECISION_SCHEMA_VERSION {
        return Err(DecisionRejection::SchemaVersion(decision.schema_version));
    }

    // References: every cited intention and span must be one the host issued.
    let mut seen = BTreeSet::new();
    for id in &decision.supporting_intentions {
        if !input.intentions.iter().any(|intention| &intention.id == id) {
            return Err(DecisionRejection::UnknownIntention(id.clone()));
        }
        if !seen.insert(id.as_str()) {
            return Err(DecisionRejection::DuplicateIntention(id.clone()));
        }
    }
    for span in &decision.cited_owner_spans {
        if !input
            .request
            .owner_spans
            .iter()
            .any(|owner| &owner.id == span)
        {
            return Err(DecisionRejection::UnknownSpan(span.clone()));
        }
    }

    // Field combinations that cannot mean anything coherent.
    match (decision.intervention, decision.continuation) {
        (Intervention::None, Continuation::Wait) => {
            return Err(DecisionRejection::Contradiction(
                "wait without an intervention",
            ));
        },
        (Intervention::Clarify | Intervention::Confirm, Continuation::Proceed) => {
            return Err(DecisionRejection::Contradiction(
                "clarify or confirm cannot proceed before the owner answers",
            ));
        },
        _ => {},
    }
    if decision.assessment == Assessment::Aligned
        && (decision.intervention != Intervention::None
            || decision.continuation != Continuation::Proceed)
    {
        return Err(DecisionRejection::Contradiction(
            "aligned must proceed with no intervention",
        ));
    }
    if decision.assessment == Assessment::AcknowledgedException
        && decision.continuation != Continuation::Proceed
    {
        return Err(DecisionRejection::Contradiction(
            "an acknowledged exception proceeds",
        ));
    }
    if decision.continuation == Continuation::Wait && decision.resolution_choices.is_empty() {
        return Err(DecisionRejection::Contradiction("wait offers no choices"));
    }
    if decision.continuation != Continuation::Wait && !decision.resolution_choices.is_empty() {
        return Err(DecisionRejection::Contradiction(
            "choices offered without waiting for them",
        ));
    }

    // Provenance: only the current owner turn can cancel work or acknowledge
    // an exception. History, assistant text and tool output cannot.
    if decision.continuation == Continuation::Cancel && decision.cited_owner_spans.is_empty() {
        return Err(DecisionRejection::MissingOwnerSpan("cancel"));
    }
    if decision.assessment == Assessment::AcknowledgedException
        && decision.cited_owner_spans.is_empty()
    {
        return Err(DecisionRejection::MissingOwnerSpan(
            "acknowledged_exception",
        ));
    }

    // Grounding: every intervention names the evidence it rests on.
    if decision.intervention != Intervention::None {
        if decision.supporting_intentions.is_empty() {
            return Err(DecisionRejection::Ungrounded(decision.intervention));
        }
        if decision.explanation.trim().is_empty() {
            return Err(DecisionRejection::MissingExplanation);
        }
    }
    if decision.explanation.chars().count() > MAX_EXPLANATION_CHARS {
        return Err(DecisionRejection::ExplanationTooLong);
    }

    // What a wait rests on: named, cited, and only for a wait.
    let mut holding = BTreeSet::new();
    for id in &decision.holding_intentions {
        if !input.intentions.iter().any(|intention| &intention.id == id) {
            return Err(DecisionRejection::UnknownIntention(id.clone()));
        }
        if !decision.supporting_intentions.contains(id) {
            return Err(DecisionRejection::HoldingNotCited(id.clone()));
        }
        if !holding.insert(id.as_str()) {
            return Err(DecisionRejection::DuplicateIntention(id.clone()));
        }
    }
    if decision.continuation == Continuation::Wait && decision.holding_intentions.is_empty() {
        return Err(DecisionRejection::Contradiction(
            "a wait names no intention it rests on",
        ));
    }
    if decision.continuation != Continuation::Wait && !decision.holding_intentions.is_empty() {
        return Err(DecisionRejection::Contradiction(
            "holding intentions without waiting",
        ));
    }

    let cited: Vec<&Intention> = decision
        .supporting_intentions
        .iter()
        .filter_map(|id| {
            input
                .intentions
                .iter()
                .find(|intention| &intention.id == id)
        })
        .collect();

    if let Some(offer) = &decision.remember_offer {
        let valid = cited
            .iter()
            .any(|intention| &intention.id == offer && !intention.assertion_status.may_hold());
        if !valid {
            return Err(DecisionRejection::InvalidRememberOffer(offer.clone()));
        }
    }

    let holding: Vec<&Intention> = cited
        .iter()
        .copied()
        .filter(|intention| decision.holding_intentions.contains(&intention.id))
        .collect();
    // A confirmation that waits, cites an explicit intention, but says it
    // rests only on weaker evidence contradicts itself: confirm is reserved
    // for explicit evidence. Where the cap would lower it all the way to
    // proceed, that mislabel would let the act through; rejecting leaves the
    // check unresolved and the act held. Under a choice policy the cap keeps
    // it waiting as a choice, so it is lowered, not rejected.
    if decision.intervention == Intervention::Confirm
        && decision.continuation == Continuation::Wait
        && !holding
            .iter()
            .any(|intention| intention.assertion_status.may_hold())
        && !inferred_choice(input, &holding)
        && cited
            .iter()
            .any(|intention| intention.assertion_status.may_hold())
    {
        return Err(DecisionRejection::Contradiction(
            "a confirmation cites explicit evidence but rests on none",
        ));
    }
    Ok(cap_force(input, decision, &holding))
}

/// Whether any of `holding` may present a choice under its effective owner
/// policy: evidence that may offer a choice, with a choice-based policy.
fn inferred_choice(input: &AlignmentInput, holding: &[&Intention]) -> bool {
    holding.iter().any(|intention| {
        intention.assertion_status.may_offer_choice()
            && matches!(
                input.policy.for_intention(intention),
                Some(
                    InterventionPolicy::ChoiceForMaterial
                        | InterventionPolicy::ConfirmBeforeException
                )
            )
    })
}

/// Lower an answer's force to what the evidence its wait rests on may carry
/// (P0 contracts §6.2). `holding` is the decision's `holding_intentions`, not
/// everything it cites:
///
/// - a hold (`clarify`, `confirm`, or a `nudge` that waits) needs at least one
///   holding intention that may hold, *or* an inferred one whose effective
///   owner policy asks for choices on material conflicts;
/// - otherwise the answer becomes an advisory nudge that proceeds, and its
///   holding list is cleared with its choices.
///
/// A held `cancel` is untouched: it rests on the owner's own current words,
/// already verified above, not on remembered evidence.
fn cap_force(
    input: &AlignmentInput,
    mut decision: AlignmentDecision,
    holding: &[&Intention],
) -> ValidatedDecision {
    if decision.continuation != Continuation::Wait {
        return ValidatedDecision {
            decision,
            downgrade: None,
        };
    }

    let any_may_hold = holding
        .iter()
        .any(|intention| intention.assertion_status.may_hold());
    if any_may_hold {
        return ValidatedDecision {
            decision,
            downgrade: None,
        };
    }

    let any_inferred_choice = inferred_choice(input, holding);
    // An owner who asked for choices on inferred preferences gets a choice —
    // never a confirmation, which is reserved for what they explicitly said.
    if any_inferred_choice {
        if decision.intervention == Intervention::Nudge {
            return ValidatedDecision {
                decision,
                downgrade: None,
            };
        }
        let downgrade = ForceDowngrade {
            from_intervention: decision.intervention,
            from_continuation: decision.continuation,
            reason: "only inferred evidence: a choice, not a confirmation".to_string(),
            rested_on: decision.holding_intentions.clone(),
        };
        decision.intervention = Intervention::Nudge;
        return ValidatedDecision {
            decision,
            downgrade: Some(downgrade),
        };
    }

    let downgrade = ForceDowngrade {
        from_intervention: decision.intervention,
        from_continuation: decision.continuation,
        reason: if holding
            .iter()
            .all(|intention| !intention.assertion_status.may_offer_choice())
        {
            "only untrusted or unknown-provenance evidence: advisory".to_string()
        } else {
            "only inferred evidence and no choice-based owner policy: advisory".to_string()
        },
        rested_on: decision.holding_intentions.clone(),
    };
    decision.intervention = Intervention::Nudge;
    decision.continuation = Continuation::Proceed;
    decision.resolution_choices.clear();
    decision.holding_intentions.clear();
    ValidatedDecision {
        decision,
        downgrade: Some(downgrade),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intention::{
        AssertionStatus, CoverageStatus, IntentionKind, IntentionLifecycle, IntentionSource,
        INTENTION_SCHEMA_VERSION,
    };

    fn intention(id: &str, status: AssertionStatus) -> Intention {
        Intention {
            schema_version: INTENTION_SCHEMA_VERSION.to_string(),
            id: id.to_string(),
            revision: "r1".to_string(),
            source: IntentionSource::NativeTier {
                tier: "preferences".to_string(),
            },
            kind: IntentionKind::Taste,
            assertion_status: status,
            statement: format!("statement {id}"),
            conditions: None,
            scope: None,
            lifecycle: IntentionLifecycle::Active,
            valid_until: None,
            evidence_refs: Vec::new(),
            intervention_policy: None,
        }
    }

    fn input(intentions: Vec<Intention>, global: Option<InterventionPolicy>) -> AlignmentInput {
        AlignmentInput {
            schema_version: ALIGNMENT_INPUT_SCHEMA_VERSION.to_string(),
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            execution_id: None,
            request: AlignmentRequest {
                revision: 1,
                text: "order dinner".to_string(),
                owner_spans: vec![OwnerSpan {
                    id: "span-1".to_string(),
                    text: "yes, chicken this once".to_string(),
                }],
            },
            intentions,
            coverage: Coverage {
                status: CoverageStatus::Complete,
                omitted_ids: Vec::new(),
                unavailable_sources: Vec::new(),
            },
            host_matched_exceptions: Vec::new(),
            action: None,
            pending_gates: Vec::new(),
            policy: EffectivePolicy {
                global,
                version: "policy.v1".to_string(),
            },
            generation: StandingGeneration::default(),
        }
    }

    fn confirm(ids: &[&str]) -> AlignmentDecision {
        AlignmentDecision {
            schema_version: ALIGNMENT_DECISION_SCHEMA_VERSION.to_string(),
            assessment: Assessment::Conflict,
            intervention: Intervention::Confirm,
            continuation: Continuation::Wait,
            supporting_intentions: ids.iter().map(|id| id.to_string()).collect(),
            holding_intentions: ids.iter().map(|id| id.to_string()).collect(),
            cited_owner_spans: Vec::new(),
            explanation: "This conflicts with what you told me.".to_string(),
            unresolved_facts: Vec::new(),
            resolution_choices: vec![ResolutionChoice::AllowOnce, ResolutionChoice::Cancel],
            remember_offer: None,
        }
    }

    #[test]
    fn explicit_evidence_keeps_its_confirmation() {
        let input = input(
            vec![intention("i1", AssertionStatus::DirectOwnerStatement)],
            None,
        );
        let validated = validate_decision(&input, confirm(&["i1"])).expect("valid");
        assert_eq!(validated.decision.intervention, Intervention::Confirm);
        assert_eq!(validated.decision.continuation, Continuation::Wait);
        assert!(validated.downgrade.is_none());
    }

    #[test]
    fn inferred_evidence_alone_never_holds_by_default() {
        let input = input(vec![intention("i1", AssertionStatus::Inferred)], None);
        let validated = validate_decision(&input, confirm(&["i1"])).expect("valid");
        assert_eq!(validated.decision.intervention, Intervention::Nudge);
        assert_eq!(validated.decision.continuation, Continuation::Proceed);
        assert!(validated.decision.resolution_choices.is_empty());
        assert!(validated.downgrade.is_some());
    }

    #[test]
    fn inferred_evidence_under_a_choice_policy_offers_a_choice_not_a_confirmation() {
        let input = input(
            vec![intention("i1", AssertionStatus::Inferred)],
            Some(InterventionPolicy::ChoiceForMaterial),
        );
        let validated = validate_decision(&input, confirm(&["i1"])).expect("valid");
        assert_eq!(validated.decision.intervention, Intervention::Nudge);
        assert_eq!(validated.decision.continuation, Continuation::Wait);
        assert!(validated.downgrade.is_some());
    }

    #[test]
    fn untrusted_evidence_stays_advisory_even_under_a_choice_policy() {
        let input = input(
            vec![intention("i1", AssertionStatus::InferredUntrusted)],
            Some(InterventionPolicy::ChoiceForMaterial),
        );
        let validated = validate_decision(&input, confirm(&["i1"])).expect("valid");
        assert_eq!(validated.decision.continuation, Continuation::Proceed);
    }

    #[test]
    fn a_hold_resting_on_explicit_evidence_keeps_its_force() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::Inferred),
                intention("i2", AssertionStatus::Confirmed),
            ],
            None,
        );
        let mut decision = confirm(&["i1", "i2"]);
        decision.holding_intentions = vec!["i2".to_string()];
        let validated = validate_decision(&input, decision).expect("valid");
        assert_eq!(validated.decision.intervention, Intervention::Confirm);
        assert!(validated.downgrade.is_none());
    }

    /// v1 let any explicit citation carry a hold. A hold that rests on the
    /// inferred conflict alone is lowered, whatever else it cites.
    #[test]
    fn an_unrelated_explicit_citation_never_lends_an_inference_force() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::Inferred),
                intention("i2", AssertionStatus::Confirmed),
            ],
            None,
        );
        // A choice that waits, resting on the inference, citing the explicit
        // rule as context: lowered to advisory under the default policy.
        let mut decision = confirm(&["i1", "i2"]);
        decision.intervention = Intervention::Nudge;
        decision.holding_intentions = vec!["i1".to_string()];
        let validated = validate_decision(&input, decision).expect("valid");
        assert_eq!(validated.decision.intervention, Intervention::Nudge);
        assert_eq!(validated.decision.continuation, Continuation::Proceed);
        assert!(validated.decision.holding_intentions.is_empty());
        // The lowered nudge still says what it was about.
        assert_eq!(
            validated.downgrade.map(|downgrade| downgrade.rested_on),
            Some(vec!["i1".to_string()])
        );
    }

    /// Lowering this would let the act through on a mislabelled answer.
    #[test]
    fn a_confirmation_resting_on_inference_beside_cited_explicit_evidence_is_rejected() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::Inferred),
                intention("i2", AssertionStatus::DirectOwnerStatement),
            ],
            None,
        );
        let mut decision = confirm(&["i1", "i2"]);
        decision.holding_intentions = vec!["i1".to_string()];
        assert!(matches!(
            validate_decision(&input, decision.clone()),
            Err(DecisionRejection::Contradiction(_))
        ));

        // Under a choice policy the cap keeps it waiting as a choice: lowered,
        // never let through, so not rejected.
        let mut choice_input = input.clone();
        choice_input.policy.global = Some(InterventionPolicy::ChoiceForMaterial);
        let validated = validate_decision(&choice_input, decision).expect("lowered to a choice");
        assert_eq!(validated.decision.intervention, Intervention::Nudge);
        assert_eq!(validated.decision.continuation, Continuation::Wait);
    }

    #[test]
    fn a_wait_names_what_it_rests_on_and_only_a_wait_does() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::DirectOwnerStatement),
                intention("i2", AssertionStatus::DirectOwnerStatement),
            ],
            None,
        );
        let mut unnamed = confirm(&["i1"]);
        unnamed.holding_intentions.clear();
        assert!(matches!(
            validate_decision(&input, unnamed),
            Err(DecisionRejection::Contradiction(_))
        ));

        let mut uncited = confirm(&["i1"]);
        uncited.holding_intentions = vec!["i2".to_string()];
        assert_eq!(
            validate_decision(&input, uncited),
            Err(DecisionRejection::HoldingNotCited("i2".to_string()))
        );

        let proceeding = AlignmentDecision {
            intervention: Intervention::Nudge,
            continuation: Continuation::Proceed,
            resolution_choices: Vec::new(),
            ..confirm(&["i1"])
        };
        assert!(matches!(
            validate_decision(&input, proceeding),
            Err(DecisionRejection::Contradiction(_))
        ));
    }

    #[test]
    fn invented_or_duplicate_evidence_is_rejected() {
        let input = input(vec![intention("i1", AssertionStatus::Inferred)], None);
        assert_eq!(
            validate_decision(&input, confirm(&["nope"])),
            Err(DecisionRejection::UnknownIntention("nope".to_string()))
        );
        assert_eq!(
            validate_decision(&input, confirm(&["i1", "i1"])),
            Err(DecisionRejection::DuplicateIntention("i1".to_string()))
        );
    }

    #[test]
    fn contradictions_never_become_proceed() {
        let input = input(
            vec![intention("i1", AssertionStatus::DirectOwnerStatement)],
            None,
        );

        let mut confirm_proceed = confirm(&["i1"]);
        confirm_proceed.continuation = Continuation::Proceed;
        confirm_proceed.resolution_choices.clear();
        assert!(matches!(
            validate_decision(&input, confirm_proceed),
            Err(DecisionRejection::Contradiction(_))
        ));

        let mut aligned_wait = confirm(&["i1"]);
        aligned_wait.assessment = Assessment::Aligned;
        assert!(matches!(
            validate_decision(&input, aligned_wait),
            Err(DecisionRejection::Contradiction(_))
        ));

        let mut wait_without_choices = confirm(&["i1"]);
        wait_without_choices.resolution_choices.clear();
        assert!(matches!(
            validate_decision(&input, wait_without_choices),
            Err(DecisionRejection::Contradiction(_))
        ));
    }

    #[test]
    fn cancel_and_acknowledged_exception_need_the_current_owner_turn() {
        let input = input(
            vec![intention("i1", AssertionStatus::DirectOwnerStatement)],
            None,
        );

        let mut cancel = confirm(&["i1"]);
        cancel.continuation = Continuation::Cancel;
        cancel.resolution_choices.clear();
        cancel.holding_intentions.clear();
        assert_eq!(
            validate_decision(&input, cancel.clone()),
            Err(DecisionRejection::MissingOwnerSpan("cancel"))
        );
        cancel.cited_owner_spans = vec!["span-1".to_string()];
        assert!(validate_decision(&input, cancel).is_ok());

        let exception = AlignmentDecision {
            assessment: Assessment::AcknowledgedException,
            intervention: Intervention::None,
            continuation: Continuation::Proceed,
            resolution_choices: Vec::new(),
            cited_owner_spans: vec!["span-from-history".to_string()],
            ..confirm(&["i1"])
        };
        assert_eq!(
            validate_decision(&input, exception),
            Err(DecisionRejection::UnknownSpan(
                "span-from-history".to_string()
            ))
        );
    }

    #[test]
    fn interventions_must_be_grounded_and_explained() {
        let input = input(
            vec![intention("i1", AssertionStatus::DirectOwnerStatement)],
            None,
        );
        assert_eq!(
            validate_decision(&input, confirm(&[])),
            Err(DecisionRejection::Ungrounded(Intervention::Confirm))
        );
        let mut unexplained = confirm(&["i1"]);
        unexplained.explanation = "  ".to_string();
        assert_eq!(
            validate_decision(&input, unexplained),
            Err(DecisionRejection::MissingExplanation)
        );
    }

    #[test]
    fn remember_offer_only_names_a_cited_inference() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::Inferred),
                intention("i2", AssertionStatus::DirectOwnerStatement),
            ],
            None,
        );
        let mut nudge = AlignmentDecision {
            intervention: Intervention::Nudge,
            continuation: Continuation::Proceed,
            resolution_choices: Vec::new(),
            remember_offer: Some("i1".to_string()),
            holding_intentions: Vec::new(),
            ..confirm(&["i1"])
        };
        assert!(validate_decision(&input, nudge.clone()).is_ok());

        nudge.remember_offer = Some("i2".to_string());
        nudge.supporting_intentions = vec!["i2".to_string()];
        assert_eq!(
            validate_decision(&input, nudge),
            Err(DecisionRejection::InvalidRememberOffer("i2".to_string()))
        );
    }
}
