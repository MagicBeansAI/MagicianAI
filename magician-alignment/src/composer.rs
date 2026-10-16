//! The structured-decision path for `action_alignment` (plan §7.3): pack
//! `action_alignment/1.1.0` asks a typed decision model (Jev, Kev, Laya) one
//! question per offered intention, and the host composes an
//! `alignment_decision.v2` from those answers.
//!
//! The model answers only the semantic question: how this act relates to this
//! one intention, and whether the owner's current turn cancels the act. How
//! strongly to intervene is not asked. The host derives it from the answer,
//! the intention's evidence force and the owner's effective policy, the same
//! rules [`crate::contract::validate_decision`] enforces on the generative
//! judge. The composed decision still goes through that validator.
//!
//! A relation that does not settle is a tie, decided by firmness: a firm
//! explicit rule asks and waits; otherwise a detail the owner's current turn
//! chose proceeds, and anything else gets a nudge that proceeds. An uncertain
//! cancellation or a missing answer leaves the whole check unresolved: the
//! caller falls back to the generative reference or holds the act.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::contract::{
    AlignmentDecision, AlignmentInput, Assessment, Continuation, Intervention, ResolutionChoice,
    ALIGNMENT_DECISION_SCHEMA_VERSION, MAX_EXPLANATION_CHARS,
};
use crate::intention::{
    AssertionStatus, Intention, IntentionKind, IntentionLifecycle, InterventionPolicy,
};

pub const ENGINE_OPERATION: &str = "action_alignment";
pub const ENGINE_PACK_VERSION: &str = "1.3.0";
/// Identity of [`project`]'s shape. A change invalidates any qualification.
pub const PROJECTION_VERSION: &str = "action_alignment_projection.v2";
pub const RELATION_QUESTION: &str = "relation";
pub const CANCEL_QUESTION: &str = "owner_cancels";
pub const SPECIFIED_QUESTION: &str = "owner_specified";
pub const FIRM_QUESTION: &str = "firm_limit";
pub const SUBJECT_QUESTION: &str = "same_subject";
pub const MUTED_QUESTION: &str = "muted";
pub const DECIDES_QUESTION: &str = "owner_decides";

/// Default confidence a relation needs to settle.
pub const THRESHOLD: f64 = 0.5;

/// Side answers each have a fixed bar that does not move with the relation
/// threshold: "the owner's current turn chose this" at or above
/// [`SPECIFIED_BAR`], and "a firm limit" at or below [`SOFT_BAR`] reads as
/// soft. `same_subject` is recorded but never overrides the relation: a rule
/// about which outsiders may receive something has no single subject person,
/// and the relation question already reads who the act is for.
pub const SPECIFIED_BAR: f64 = 0.5;
pub const SOFT_BAR: f64 = 0.5;

/// Letting an act through past the owner's own words needs more confidence
/// than the threshold alone: an explicit or confirmed intention settles as
/// "does not apply / fits" only when that pooled mass reaches at least this
/// bar. Below it the answer is a tie (see [`tie_effect`]). Missing an
/// explicit conflict costs more than one more question.
pub const EXPLICIT_BENIGN_BAR: f64 = 0.6;

/// P(another of the owner's statements says not to raise this intention for
/// this act) at or above which it is muted (see [`compose`]).
pub const MUTE_BAR: f64 = 0.5;

/// P(the current owner turn makes the one decision a standing instruction
/// reserves for the owner: agreeing to a request, or what a reply says) at or
/// above which that instruction is satisfied by the turn (see [`force_of`]).
/// Owner ruling 2026-10-11: rules guarding time, money, health, diet, safety
/// or a kind of act are never satisfied by the owner asking for the act.
/// 0.7, not 0.5: Jev put two plain check-first conflicts at 0.50 and 0.51,
/// and the intended cases at 0.74 and 0.82 (2026-10-11 measurement).
pub const DECIDES_BAR: f64 = 0.7;

/// The bars the composer judges answers against. The defaults are the
/// qualified values; they are calibrated to one model's probabilities, so a
/// different model needs them re-measured. Configured under
/// `preference_alignment.composer`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ComposerBars {
    /// Confidence a relation needs to settle.
    pub threshold: f64,
    /// Pooled "does not apply / fits" mass an explicit or confirmed intention
    /// needs before an act passes it ([`EXPLICIT_BENIGN_BAR`]).
    pub explicit_benign: f64,
    /// P(the owner's current turn chose this detail) that counts as chosen.
    pub specified: f64,
    /// P(firm limit) at or below which an intention reads as soft.
    pub soft: f64,
    /// P(muted by another owner statement) at or above which an intention
    /// is muted ([`MUTE_BAR`]).
    pub mute: f64,
    /// P(the owner's turn makes the decision a rule reserves for them) at or
    /// above which that rule is satisfied ([`DECIDES_BAR`]).
    pub decides: f64,
}

impl Default for ComposerBars {
    fn default() -> Self {
        Self {
            threshold: THRESHOLD,
            explicit_benign: EXPLICIT_BENIGN_BAR,
            specified: SPECIFIED_BAR,
            soft: SOFT_BAR,
            mute: MUTE_BAR,
            decides: DECIDES_BAR,
        }
    }
}

impl ComposerBars {
    /// The same bars at another relation threshold.
    pub fn at_threshold(self, threshold: f64) -> Self {
        Self { threshold, ..self }
    }

    /// Every bar must be a probability strictly inside (0, 1), between 0.05
    /// and 0.95: a bar of 0 or 1 would make one answer decide everything.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("threshold", self.threshold),
            ("explicit_benign", self.explicit_benign),
            ("specified", self.specified),
            ("soft", self.soft),
            ("mute", self.mute),
            ("decides", self.decides),
        ] {
            if !value.is_finite() || !(0.05..=0.95).contains(&value) {
                return Err(format!("{name} must be between 0.05 and 0.95, got {value}"));
            }
        }
        Ok(())
    }
}

/// How the act relates to one intention: the `relation` options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    NotApplicable,
    Consistent,
    Conflict,
    Unclear,
    AcknowledgedException,
}

/// One intention's answers: the `relation` distribution, P(the current owner
/// turn cancels the act), P(it explicitly chooses the detail that matters to
/// this intention), P(the intention is a firm limit) and P(it is about the
/// person this act is for or affects).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentionHeads {
    pub relation: BTreeMap<Relation, f64>,
    pub owner_cancels: f64,
    pub owner_specified: f64,
    pub firm_limit: f64,
    pub same_subject: f64,
    /// P(another of the owner's offered statements says not to raise this
    /// one for this act). Absent in answers to pack 1.1.0, which read as 0.
    #[serde(default)]
    pub muted: f64,
    /// P(the current owner turn makes the one decision this intention
    /// reserves for the owner). Absent before pack 1.3.0, read as 0.
    #[serde(default)]
    pub owner_decides: f64,
}

/// Why a composition did not resolve. Every variant means "not decided".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeUnresolved {
    #[error("no answer for intention `{0}`")]
    MissingAnswer(String),
    #[error("`{question}` for intention `{intention}` is below the confidence threshold")]
    Uncertain {
        intention: String,
        question: &'static str,
    },
    #[error("cancel or an acknowledged exception needs a span of the current owner turn")]
    NoOwnerSpan,
    #[error("no offered intention to ask about")]
    NoIntentions,
    #[error("the answer for intention `{0}` is not a valid probability distribution")]
    InvalidAnswer(String),
    #[error("invalid composer bars: {0}")]
    InvalidBars(String),
}

/// The shared context and one item per offered intention, in input order.
/// Item ids are the host-issued intention ids.
pub fn project(input: &AlignmentInput) -> (Value, Vec<(String, Value)>) {
    let owner_turn = input
        .request
        .owner_spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut context = Map::new();
    context.insert("conversation".into(), json!(input.request.text));
    context.insert("owner_turn".into(), json!(owner_turn));
    if let Some(action) = &input.action {
        let mut act = Map::new();
        act.insert("capability".into(), json!(action.capability));
        act.insert("action".into(), json!(action.action));
        act.insert("args".into(), action.normalized_args.clone());
        if !action.recipients.is_empty() {
            act.insert("recipients".into(), json!(action.recipients));
        }
        if let Some(class) = &action.outward_class {
            act.insert("outward_class".into(), json!(class));
        }
        act.insert("consequence_class".into(), json!(action.consequence_class));
        if !action.model_interpretations.is_null() {
            act.insert(
                "model_interpretations".into(),
                action.model_interpretations.clone(),
            );
        }
        context.insert("action".into(), Value::Object(act));
    }
    // The owner's other offered statements, so an item can see one that
    // mutes it. Wording and kind only: never evidence force. Only when there
    // is another statement to see: a lone intention cannot be muted, and
    // repeating it in context shifted Jev's relation answers (2026-10-11
    // ablation: dev 0.894 -> 0.866, all losses single-intention cases).
    if input.intentions.len() > 1 {
        context.insert(
            "intentions".into(),
            json!(input
                .intentions
                .iter()
                .map(|intention| json!({"statement": intention.statement, "kind": intention.kind}))
                .collect::<Vec<_>>()),
        );
    }
    let items = input
        .intentions
        .iter()
        .map(|intention| {
            let mut item = Map::new();
            item.insert("statement".into(), json!(intention.statement));
            if let Some(conditions) = &intention.conditions {
                item.insert("conditions".into(), json!(conditions));
            }
            item.insert("kind".into(), json!(intention.kind));
            (intention.id.clone(), Value::Object(item))
        })
        .collect();
    (Value::Object(context), items)
}

/// What one intention's answer asks of the act, strongest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Effect {
    None,
    Acknowledged,
    NudgeProceed,
    NudgeWait,
    ClarifyWait,
    ConfirmWait,
}

fn p(heads: &IntentionHeads, relation: Relation) -> f64 {
    heads.relation.get(&relation).copied().unwrap_or(0.0)
}

/// The relation an answer settles on at `threshold`, or `None` when it is
/// uncertain. "Does not apply" and "fits" both mean no intervention, so
/// their mass is pooled; the pooled answer is whichever of the two is larger.
/// For evidence that may hold, the pooled mass must also reach
/// the explicit benign bar.
fn settled(heads: &IntentionHeads, intention: &Intention, bars: &ComposerBars) -> Option<Relation> {
    let threshold = bars.threshold;
    let benign = p(heads, Relation::NotApplicable) + p(heads, Relation::Consistent);
    let benign_bar = if intention.assertion_status.may_hold() {
        threshold.max(bars.explicit_benign)
    } else {
        threshold
    };
    if benign >= benign_bar {
        return Some(
            if p(heads, Relation::Consistent) >= p(heads, Relation::NotApplicable) {
                Relation::Consistent
            } else {
                Relation::NotApplicable
            },
        );
    }
    [
        Relation::Conflict,
        Relation::Unclear,
        Relation::AcknowledgedException,
    ]
    .into_iter()
    .find(|relation| p(heads, *relation) >= threshold)
}

/// The response the host allows for one intention's answer, from its
/// evidence force, lifecycle and kind and the owner's effective policy. The
/// force rules are the ones `validate_decision` applies to any judge; the
/// model's side questions only ever soften an intervention where the
/// evidence is weak, or choose between confirm and a choice where it is
/// explicit, each against its fixed bar.
fn effect_of(
    input: &AlignmentInput,
    intention: &Intention,
    relation: Relation,
    heads: &IntentionHeads,
    bars: &ComposerBars,
) -> Effect {
    capped(input, force_of(input, intention, relation, heads, bars))
}

/// A reply or a private local step holds nothing outward: the most it earns
/// is a mention.
fn capped(input: &AlignmentInput, effect: Effect) -> Effect {
    let outward = input
        .action
        .as_ref()
        .is_some_and(|action| action.outward_class.is_some());
    if !outward && effect > Effect::NudgeProceed {
        Effect::NudgeProceed
    } else {
        effect
    }
}

fn force_of(
    input: &AlignmentInput,
    intention: &Intention,
    relation: Relation,
    heads: &IntentionHeads,
    bars: &ComposerBars,
) -> Effect {
    let policy = input.policy.for_intention(intention);
    let status = intention.assertion_status;
    let choice_policy = matches!(
        policy,
        Some(InterventionPolicy::ChoiceForMaterial | InterventionPolicy::ConfirmBeforeException)
    );
    // Chosen by the owner's own current words.
    let specified = heads.owner_specified >= bars.specified;
    // Soft only when not a firm limit; a check-first instruction is firm by
    // kind.
    let soft =
        heads.firm_limit <= bars.soft && intention.kind != IntentionKind::StandingInstruction;
    let governs = !matches!(relation, Relation::NotApplicable);

    if status.may_hold() {
        // Two of the owner's own statements disagree: ask which holds.
        if intention.lifecycle == IntentionLifecycle::Unresolved
            && governs
            && relation != Relation::Consistent
        {
            return Effect::ClarifyWait;
        }
        return match relation {
            Relation::NotApplicable | Relation::Consistent => Effect::None,
            Relation::AcknowledgedException => Effect::Acknowledged,
            // The owner's own turn makes the one decision a standing
            // instruction reserves for them ("tell Petrov yes"): the check it
            // asks for has happened. Never for limits on time, money, health
            // or a kind of act; the pack question excludes those.
            Relation::Conflict
                if intention.kind == IntentionKind::StandingInstruction
                    && heads.owner_decides >= bars.decides
                    && !input.request.owner_spans.is_empty() =>
            {
                Effect::Acknowledged
            },
            Relation::Conflict => match policy {
                Some(InterventionPolicy::Advisory) => Effect::NudgeProceed,
                Some(InterventionPolicy::ChoiceForMaterial) => Effect::NudgeWait,
                Some(InterventionPolicy::ConfirmBeforeException) => Effect::ConfirmWait,
                None if soft => Effect::NudgeWait,
                None => Effect::ConfirmWait,
            },
            Relation::Unclear => match policy {
                Some(InterventionPolicy::Advisory) => Effect::NudgeProceed,
                _ => Effect::ClarifyWait,
            },
        };
    }
    // Weaker evidence yields to what the owner explicitly asked for now, and
    // an acknowledgement of it needs no exception.
    if specified || matches!(relation, Relation::AcknowledgedException) {
        return Effect::None;
    }
    match relation {
        Relation::NotApplicable | Relation::Consistent | Relation::AcknowledgedException => {
            Effect::None
        },
        Relation::Conflict if status.may_offer_choice() && choice_policy => Effect::NudgeWait,
        // Inferred evidence offers a choice, never a question that holds like
        // a confirmation: the same cap `validate_decision` applies.
        Relation::Unclear if status.may_offer_choice() && choice_policy => Effect::NudgeWait,
        Relation::Conflict | Relation::Unclear => Effect::NudgeProceed,
    }
}

/// Whether another of the owner's statements mutes this intention for this
/// act ("don't bring up my deposit goal for anything under $300"). Only an
/// explicit or confirmed standing instruction among the offered intentions
/// can mute, and a firm explicit rule is never muted: a mute quiets goals,
/// tastes and inferences, not a "check with me first".
fn muted(
    input: &AlignmentInput,
    intention: &Intention,
    heads: &IntentionHeads,
    bars: &ComposerBars,
) -> bool {
    let firm_explicit = intention.assertion_status.may_hold()
        && (heads.firm_limit > bars.soft || intention.kind == IntentionKind::StandingInstruction);
    let owner_can_mute = input.intentions.iter().any(|other| {
        other.id != intention.id
            && other.kind == IntentionKind::StandingInstruction
            && other.assertion_status.may_hold()
    });
    heads.muted >= bars.mute && owner_can_mute && !firm_explicit
}

/// The response for an intention whose relation did not settle: a firm
/// explicit rule asks (or, under an advisory policy, is mentioned);
/// otherwise the owner's current choice proceeds, and anything else is
/// mentioned on the way. A tie never intervenes harder than a settled
/// `unclear` would.
fn tie_effect(
    input: &AlignmentInput,
    intention: &Intention,
    heads: &IntentionHeads,
    bars: &ComposerBars,
) -> Effect {
    let firm =
        heads.firm_limit > bars.soft || intention.kind == IntentionKind::StandingInstruction;
    let advisory = matches!(
        input.policy.for_intention(intention),
        Some(InterventionPolicy::Advisory)
    );
    if intention.assertion_status.may_hold() && firm {
        if advisory {
            Effect::NudgeProceed
        } else {
            Effect::ClarifyWait
        }
    } else if heads.owner_specified >= bars.specified {
        Effect::None
    } else {
        Effect::NudgeProceed
    }
}

/// Every answer a probability, and the relation a distribution: anything else
/// is malformed and decides nothing.
fn well_formed(heads: &IntentionHeads) -> bool {
    let probability = |value: f64| value.is_finite() && (0.0..=1.0).contains(&value);
    let mass: f64 = heads.relation.values().sum();
    heads.relation.values().all(|value| probability(*value))
        && (0.97..=1.03).contains(&mass)
        && [
            heads.owner_cancels,
            heads.owner_specified,
            heads.firm_limit,
            heads.same_subject,
            heads.muted,
            heads.owner_decides,
        ]
        .into_iter()
        .all(probability)
}

fn quoted(statements: &[&str]) -> String {
    statements
        .iter()
        .map(|statement| format!("\u{201c}{statement}\u{201d}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn clip(text: String) -> String {
    if text.chars().count() <= MAX_EXPLANATION_CHARS {
        return text;
    }
    let mut clipped: String = text.chars().take(MAX_EXPLANATION_CHARS - 1).collect();
    clipped.push('\u{2026}');
    clipped
}

fn statements<'a>(intentions: &[&'a Intention]) -> Vec<&'a str> {
    intentions
        .iter()
        .map(|intention| intention.statement.as_str())
        .collect()
}

fn ids(intentions: &[&Intention]) -> Vec<String> {
    intentions.iter().map(|intention| intention.id.clone()).collect()
}

/// Compose a decision from per-intention answers against `bars`. The result
/// is unvalidated: the caller runs it through `validate_decision` like any
/// judge answer.
pub fn compose(
    input: &AlignmentInput,
    answers: &BTreeMap<String, IntentionHeads>,
    bars: &ComposerBars,
) -> Result<AlignmentDecision, ComposeUnresolved> {
    bars.validate().map_err(ComposeUnresolved::InvalidBars)?;
    let threshold = bars.threshold;
    if input.intentions.is_empty() {
        return Err(ComposeUnresolved::NoIntentions);
    }
    let owner_spans: Vec<String> = input
        .request
        .owner_spans
        .iter()
        .map(|span| span.id.clone())
        .collect();
    let matched: Vec<&str> = input
        .host_matched_exceptions
        .iter()
        .flat_map(|exception| exception.intention_ids.iter().map(String::as_str))
        .collect();

    let mut cancels = 0.0_f64;
    let mut effects: Vec<(&Intention, Relation, Effect)> = Vec::new();
    for intention in &input.intentions {
        let heads = answers
            .get(&intention.id)
            .ok_or_else(|| ComposeUnresolved::MissingAnswer(intention.id.clone()))?;
        if !well_formed(heads) {
            return Err(ComposeUnresolved::InvalidAnswer(intention.id.clone()));
        }
        cancels = cancels.max(heads.owner_cancels);
        let settled = settled(heads, intention, bars);
        let relation = settled.unwrap_or(Relation::Unclear);
        // An exception the host already matched to this scope and act is the
        // owner's recorded answer: it does not intervene again.
        let effect = if matched.contains(&intention.id.as_str()) || muted(input, intention, heads, bars) {
            Effect::None
        } else if settled.is_some() {
            effect_of(input, intention, relation, heads, bars)
        } else {
            capped(input, tie_effect(input, intention, heads, bars))
        };
        effects.push((intention, relation, effect));
    }

    // The owner's current turn cancelling the act outranks every preference.
    if cancels >= threshold {
        if owner_spans.is_empty() {
            return Err(ComposeUnresolved::NoOwnerSpan);
        }
        let any_conflict = effects
            .iter()
            .any(|(_, relation, _)| *relation == Relation::Conflict);
        return Ok(decision(
            if any_conflict {
                Assessment::Conflict
            } else {
                Assessment::Unclear
            },
            Intervention::None,
            Continuation::Cancel,
            Vec::new(),
            Vec::new(),
            owner_spans,
            "Your current message cancels this action.".to_string(),
            Vec::new(),
            None,
        ));
    }
    if cancels > 1.0 - threshold {
        let intention = input
            .intentions
            .first()
            .map(|intention| intention.id.clone())
            .unwrap_or_default();
        return Err(ComposeUnresolved::Uncertain {
            intention,
            question: CANCEL_QUESTION,
        });
    }

    let strongest = effects
        .iter()
        .map(|(_, _, effect)| *effect)
        .max()
        .unwrap_or(Effect::None);
    let with = |wanted: &[Effect]| -> Vec<&Intention> {
        effects
            .iter()
            .filter(|(_, _, effect)| wanted.contains(effect))
            .map(|(intention, _, _)| *intention)
            .collect()
    };
    // Everything that asks for an intervention is cited; a wait rests on the
    // intentions that call for that wait.
    let mut intervening = with(&[
        Effect::NudgeProceed,
        Effect::NudgeWait,
        Effect::ClarifyWait,
        Effect::ConfirmWait,
    ]);
    // The other side of a tradeoff: an explicit statement the act fits, cited
    // beside a choice or a question so the owner sees both. For a question
    // about two disputed statements, both are the question.
    if matches!(strongest, Effect::NudgeWait | Effect::ClarifyWait) {
        for (intention, relation, _) in &effects {
            let disputed = strongest == Effect::ClarifyWait
                && intention.lifecycle == IntentionLifecycle::Unresolved;
            let other_side = strongest == Effect::NudgeWait;
            if *relation == Relation::Consistent
                && intention.assertion_status.may_hold()
                && (disputed || other_side)
                && !intervening.iter().any(|cited| cited.id == intention.id)
            {
                intervening.push(intention);
            }
        }
    }
    let conflicting = effects.iter().any(|(intention, relation, effect)| {
        *relation == Relation::Conflict
            && *effect != Effect::None
            && intention.assertion_status.may_offer_choice()
    });
    // Offer to remember an inferred intention the owner is being nudged about.
    let remember_offer = intervening
        .iter()
        .find(|intention| intention.assertion_status == AssertionStatus::Inferred)
        .map(|intention| intention.id.clone());

    Ok(match strongest {
        Effect::None => decision(
            Assessment::Aligned,
            Intervention::None,
            Continuation::Proceed,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            String::new(),
            Vec::new(),
            None,
        ),
        Effect::Acknowledged => {
            if owner_spans.is_empty() {
                return Err(ComposeUnresolved::NoOwnerSpan);
            }
            let acknowledged = with(&[Effect::Acknowledged]);
            decision(
                Assessment::AcknowledgedException,
                Intervention::None,
                Continuation::Proceed,
                ids(&acknowledged),
                Vec::new(),
                owner_spans,
                format!(
                    "You asked for this knowing it goes against {}.",
                    quoted(&statements(&acknowledged))
                ),
                Vec::new(),
                None,
            )
        },
        Effect::NudgeProceed => decision(
            if conflicting {
                Assessment::Conflict
            } else {
                Assessment::Unclear
            },
            Intervention::Nudge,
            Continuation::Proceed,
            ids(&intervening),
            Vec::new(),
            Vec::new(),
            format!(
                "Worth a look: this may not fit {}.",
                quoted(&statements(&intervening))
            ),
            Vec::new(),
            remember_offer,
        ),
        Effect::NudgeWait => {
            let holding = with(&[Effect::NudgeWait]);
            // A choice offered because the fit is unclear is not a conflict.
            let conflict = effects.iter().any(|(_, relation, effect)| {
                *effect == Effect::NudgeWait && *relation == Relation::Conflict
            });
            decision(
                if conflict {
                    Assessment::Conflict
                } else {
                    Assessment::Unclear
                },
                Intervention::Nudge,
                Continuation::Wait,
                ids(&intervening),
                ids(&holding),
                Vec::new(),
                format!(
                    "This {} {}. Which would you like?",
                    if conflict { "doesn't fit" } else { "may not fit" },
                    quoted(&statements(&holding))
                ),
                Vec::new(),
                remember_offer,
            )
        },
        Effect::ClarifyWait => {
            let holding = with(&[Effect::ClarifyWait]);
            let unresolved = statements(&holding)
                .into_iter()
                .map(|statement| {
                    format!("Whether \u{201c}{statement}\u{201d} applies to this action.")
                })
                .collect();
            decision(
                Assessment::Unclear,
                Intervention::Clarify,
                Continuation::Wait,
                ids(&intervening),
                ids(&holding),
                Vec::new(),
                format!(
                    "I can't tell whether this fits {}.",
                    quoted(&statements(&intervening))
                ),
                unresolved,
                None,
            )
        },
        Effect::ConfirmWait => {
            let holding = with(&[Effect::ConfirmWait]);
            decision(
                Assessment::Conflict,
                Intervention::Confirm,
                Continuation::Wait,
                ids(&intervening),
                ids(&holding),
                Vec::new(),
                format!("This goes against {}.", quoted(&statements(&holding))),
                Vec::new(),
                None,
            )
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn decision(
    assessment: Assessment,
    intervention: Intervention,
    continuation: Continuation,
    supporting_intentions: Vec<String>,
    holding_intentions: Vec<String>,
    cited_owner_spans: Vec<String>,
    explanation: String,
    unresolved_facts: Vec<String>,
    remember_offer: Option<String>,
) -> AlignmentDecision {
    let resolution_choices = if continuation == Continuation::Wait {
        vec![ResolutionChoice::AllowOnce, ResolutionChoice::Cancel]
    } else {
        Vec::new()
    };
    AlignmentDecision {
        schema_version: ALIGNMENT_DECISION_SCHEMA_VERSION.to_string(),
        assessment,
        intervention,
        continuation,
        supporting_intentions,
        holding_intentions,
        cited_owner_spans,
        explanation: clip(explanation),
        unresolved_facts,
        resolution_choices,
        remember_offer,
    }
}

/// Read one engine item's answers into [`IntentionHeads`]. `None` when an
/// answer is missing, of the wrong type, or names an unknown option.
pub fn heads_from_answers(
    answers: &BTreeMap<
        decision_engine_contract::primitives::QuestionId,
        decision_engine_contract::request::Answer,
    >,
) -> Option<IntentionHeads> {
    use decision_engine_contract::primitives::QuestionId;
    use decision_engine_contract::request::Answer;
    let relation = match answers.get(&QuestionId::new(RELATION_QUESTION))? {
        Answer::Choice { probabilities, .. } => {
            let mut relation = BTreeMap::new();
            for (option, p) in probabilities {
                let option: Relation =
                    serde_json::from_value(Value::String(option.as_str().to_string())).ok()?;
                if !p.is_finite() || !(0.0..=1.0).contains(p) {
                    return None;
                }
                relation.insert(option, *p);
            }
            relation
        },
        _ => return None,
    };
    let noul = |question: &str| match answers.get(&QuestionId::new(question))? {
        Answer::Noul { noul } if noul.is_finite() && (0.0..=1.0).contains(noul) => Some(*noul),
        _ => None,
    };
    Some(IntentionHeads {
        relation,
        owner_cancels: noul(CANCEL_QUESTION)?,
        owner_specified: noul(SPECIFIED_QUESTION)?,
        firm_limit: noul(FIRM_QUESTION)?,
        same_subject: noul(SUBJECT_QUESTION)?,
        // Pack 1.1.0 has no muting question; a 1.2.0 answer must carry it.
        muted: match answers.get(&QuestionId::new(MUTED_QUESTION)) {
            None => 0.0,
            Some(_) => noul(MUTED_QUESTION)?,
        },
        // Pack 1.3.0 adds it; earlier answers read as "no".
        owner_decides: match answers.get(&QuestionId::new(DECIDES_QUESTION)) {
            None => 0.0,
            Some(_) => noul(DECIDES_QUESTION)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        validate_decision, ActionFacts, AlignmentRequest, EffectivePolicy, OwnerSpan,
        ALIGNMENT_INPUT_SCHEMA_VERSION,
    };
    use crate::intention::{
        Coverage, IntentionSource, StandingGeneration, INTENTION_SCHEMA_VERSION,
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
                text: "user: order dinner".to_string(),
                owner_spans: vec![OwnerSpan {
                    id: "span-1".to_string(),
                    text: "order dinner".to_string(),
                }],
            },
            intentions,
            coverage: Coverage::complete(),
            host_matched_exceptions: Vec::new(),
            action: Some(ActionFacts {
                binding_id: "binding-1".to_string(),
                capability: "food_delivery".to_string(),
                action: "place_order".to_string(),
                normalized_args: json!({"items": ["chicken biryani"]}),
                recipients: Vec::new(),
                outward_class: Some("commitment_or_transaction".to_string()),
                consequence_class: "commitment_or_transaction".to_string(),
                model_interpretations: Value::Null,
            }),
            pending_gates: Vec::new(),
            policy: EffectivePolicy {
                global,
                version: "policy.v1".to_string(),
            },
            generation: StandingGeneration::default(),
        }
    }

    fn heads(relation: Relation, p: f64, cancels: f64) -> IntentionHeads {
        let mut distribution = BTreeMap::new();
        distribution.insert(relation, p);
        distribution.insert(
            if relation == Relation::Consistent {
                Relation::Conflict
            } else {
                Relation::Consistent
            },
            1.0 - p,
        );
        IntentionHeads {
            relation: distribution,
            owner_cancels: cancels,
            owner_specified: 0.0,
            firm_limit: 1.0,
            same_subject: 1.0,
            muted: 0.0,
            owner_decides: 0.0,
        }
    }

    fn at(threshold: f64) -> ComposerBars {
        ComposerBars::default().at_threshold(threshold)
    }

    fn answers(pairs: &[(&str, IntentionHeads)]) -> BTreeMap<String, IntentionHeads> {
        pairs
            .iter()
            .map(|(id, heads)| (id.to_string(), heads.clone()))
            .collect()
    }

    fn composed(
        input: &AlignmentInput,
        pairs: &[(&str, IntentionHeads)],
    ) -> AlignmentDecision {
        let decision = compose(input, &answers(pairs), &at(0.7)).expect("composes");
        let validated = validate_decision(input, decision.clone()).expect("validates");
        assert_eq!(validated.downgrade, None, "the composer never needs lowering");
        decision
    }

    #[test]
    fn an_explicit_conflict_holds_for_confirmation() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let decision = composed(&input, &[("i1", heads(Relation::Conflict, 0.9, 0.0))]);
        assert_eq!(decision.intervention, Intervention::Confirm);
        assert_eq!(decision.continuation, Continuation::Wait);
        assert_eq!(decision.holding_intentions, vec!["i1"]);
    }

    #[test]
    fn an_inferred_conflict_nudges_and_waits_only_under_a_choice_policy() {
        let inferred = vec![intention("i1", AssertionStatus::Inferred)];
        let default = composed(
            &input(inferred.clone(), None),
            &[("i1", heads(Relation::Conflict, 0.9, 0.0))],
        );
        assert_eq!(
            (default.intervention, default.continuation),
            (Intervention::Nudge, Continuation::Proceed)
        );
        let choice = composed(
            &input(inferred, Some(InterventionPolicy::ChoiceForMaterial)),
            &[("i1", heads(Relation::Conflict, 0.9, 0.0))],
        );
        assert_eq!(
            (choice.intervention, choice.continuation),
            (Intervention::Nudge, Continuation::Wait)
        );
    }

    #[test]
    fn untrusted_evidence_only_advises() {
        let input = input(
            vec![intention("i1", AssertionStatus::InferredUntrusted)],
            Some(InterventionPolicy::ConfirmBeforeException),
        );
        let decision = composed(&input, &[("i1", heads(Relation::Conflict, 0.95, 0.0))]);
        assert_eq!(
            (decision.intervention, decision.continuation),
            (Intervention::Nudge, Continuation::Proceed)
        );
    }

    #[test]
    fn the_strongest_effect_wins_and_the_wait_rests_only_on_it() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::Inferred),
                intention("i2", AssertionStatus::DirectOwnerStatement),
            ],
            None,
        );
        let decision = composed(
            &input,
            &[
                ("i1", heads(Relation::Conflict, 0.9, 0.0)),
                ("i2", heads(Relation::Conflict, 0.9, 0.0)),
            ],
        );
        assert_eq!(decision.intervention, Intervention::Confirm);
        assert_eq!(decision.supporting_intentions, vec!["i1", "i2"]);
        assert_eq!(decision.holding_intentions, vec!["i2"]);
    }

    #[test]
    fn an_unclear_explicit_intention_asks_and_waits() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let decision = composed(&input, &[("i1", heads(Relation::Unclear, 0.8, 0.0))]);
        assert_eq!(decision.intervention, Intervention::Clarify);
        assert_eq!(decision.unresolved_facts.len(), 1);
    }

    #[test]
    fn an_acknowledged_exception_proceeds_citing_the_owner_turn() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let decision = composed(
            &input,
            &[("i1", heads(Relation::AcknowledgedException, 0.9, 0.0))],
        );
        assert_eq!(decision.assessment, Assessment::AcknowledgedException);
        assert_eq!(decision.continuation, Continuation::Proceed);
        assert_eq!(decision.cited_owner_spans, vec!["span-1"]);
    }

    #[test]
    fn an_unacknowledged_conflict_outranks_an_acknowledged_one() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::DirectOwnerStatement),
                intention("i2", AssertionStatus::DirectOwnerStatement),
            ],
            None,
        );
        let decision = composed(
            &input,
            &[
                ("i1", heads(Relation::AcknowledgedException, 0.9, 0.0)),
                ("i2", heads(Relation::Conflict, 0.9, 0.0)),
            ],
        );
        assert_eq!(decision.intervention, Intervention::Confirm);
        assert_eq!(decision.holding_intentions, vec!["i2"]);
    }

    #[test]
    fn the_owner_cancelling_cancels_with_the_owner_span() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let decision = composed(&input, &[("i1", heads(Relation::Consistent, 0.9, 0.95))]);
        assert_eq!(decision.continuation, Continuation::Cancel);
        assert_eq!(decision.cited_owner_spans, vec!["span-1"]);
    }

    #[test]
    fn not_applicable_and_consistent_pool_into_no_intervention() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let mut distribution = BTreeMap::new();
        distribution.insert(Relation::NotApplicable, 0.45);
        distribution.insert(Relation::Consistent, 0.45);
        distribution.insert(Relation::Conflict, 0.10);
        let decision = composed(
            &input,
            &[(
                "i1",
                IntentionHeads {
                    relation: distribution,
                    owner_cancels: 0.0,
                    owner_specified: 0.0,
                    firm_limit: 1.0,
                    same_subject: 1.0,
                    muted: 0.0,
                    owner_decides: 0.0,
                },
            )],
        );
        assert_eq!(decision.assessment, Assessment::Aligned);
    }

    #[test]
    fn an_unsettled_firm_rule_asks_and_an_unsure_cancel_stays_unresolved() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let decision = composed(&input, &[("i1", heads(Relation::Consistent, 0.6, 0.0))]);
        assert_eq!(
            (decision.intervention, decision.continuation),
            (Intervention::Clarify, Continuation::Wait)
        );
        assert!(matches!(
            compose(&input, &answers(&[("i1", heads(Relation::Consistent, 0.9, 0.5))]), &at(0.7)),
            Err(ComposeUnresolved::Uncertain {
                question: CANCEL_QUESTION,
                ..
            })
        ));
        assert_eq!(
            compose(&input, &BTreeMap::new(), &at(0.7)),
            Err(ComposeUnresolved::MissingAnswer("i1".to_string()))
        );
    }

    #[test]
    fn letting_an_act_past_explicit_words_needs_the_higher_bar() {
        let mut leaning_fits = BTreeMap::new();
        leaning_fits.insert(Relation::Consistent, 0.55);
        leaning_fits.insert(Relation::Conflict, 0.45);
        let heads = IntentionHeads {
            relation: leaning_fits,
            owner_cancels: 0.0,
            owner_specified: 0.0,
            firm_limit: 1.0,
            same_subject: 1.0,
            muted: 0.0,
            owner_decides: 0.0,
        };
        let explicit = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let decision = compose(&explicit, &answers(&[("i1", heads.clone())]), &at(0.5)).unwrap();
        assert_eq!(decision.continuation, Continuation::Wait);
        // Inferred evidence never holds, so the threshold alone settles it.
        let inferred = input(vec![intention("i1", AssertionStatus::Inferred)], None);
        let decision = compose(&inferred, &answers(&[("i1", heads)]), &at(0.5)).unwrap();
        assert_eq!(decision.assessment, Assessment::Aligned);
    }

    #[test]
    fn a_soft_explicit_preference_offers_a_choice_and_a_firm_one_confirms() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let mut soft = heads(Relation::Conflict, 0.9, 0.0);
        soft.firm_limit = 0.1;
        let decision = composed(&input, &[("i1", soft.clone())]);
        assert_eq!(
            (decision.intervention, decision.continuation),
            (Intervention::Nudge, Continuation::Wait)
        );
        // Leaning firm: confirm.
        soft.firm_limit = 0.6;
        let decision = composed(&input, &[("i1", soft)]);
        assert_eq!(decision.intervention, Intervention::Confirm);
    }

    #[test]
    fn a_check_first_instruction_is_firm_whatever_the_model_says() {
        let mut instruction = intention("i1", AssertionStatus::DirectOwnerStatement);
        instruction.kind = IntentionKind::StandingInstruction;
        let input = input(vec![instruction], None);
        let mut soft = heads(Relation::Conflict, 0.9, 0.0);
        soft.firm_limit = 0.0;
        assert_eq!(composed(&input, &[("i1", soft)]).intervention, Intervention::Confirm);
    }

    #[test]
    fn weak_evidence_yields_to_what_the_owner_just_asked_for() {
        for status in [
            AssertionStatus::Inferred,
            AssertionStatus::InferredUntrusted,
            AssertionStatus::UnknownProvenance,
        ] {
            let input = input(vec![intention("i1", status)], None);
            let mut chosen = heads(Relation::Conflict, 0.9, 0.0);
            chosen.owner_specified = 0.9;
            let decision = composed(&input, &[("i1", chosen)]);
            assert_eq!(decision.assessment, Assessment::Aligned, "{status:?}");
            assert!(decision.supporting_intentions.is_empty());
        }
        // An explicit statement does not: asking for the act is not consent.
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let mut chosen = heads(Relation::Conflict, 0.9, 0.0);
        chosen.owner_specified = 0.95;
        assert_eq!(composed(&input, &[("i1", chosen)]).intervention, Intervention::Confirm);
    }

    #[test]
    fn an_inferred_nudge_offers_to_remember_it() {
        let input = input(vec![intention("i1", AssertionStatus::Inferred)], None);
        let decision = composed(&input, &[("i1", heads(Relation::Conflict, 0.9, 0.0))]);
        assert_eq!(decision.remember_offer.as_deref(), Some("i1"));
    }

    #[test]
    fn untrusted_evidence_is_only_ever_an_unclear_nudge() {
        let input = input(vec![intention("i1", AssertionStatus::InferredUntrusted)], None);
        let decision = composed(&input, &[("i1", heads(Relation::Conflict, 0.95, 0.0))]);
        assert_eq!(decision.assessment, Assessment::Unclear);
        assert_eq!(decision.remember_offer, None);
    }

    #[test]
    fn disputed_statements_are_asked_about_together() {
        let mut off = intention("i-off", AssertionStatus::DirectOwnerStatement);
        let mut on = intention("i-on", AssertionStatus::DirectOwnerStatement);
        off.lifecycle = IntentionLifecycle::Unresolved;
        on.lifecycle = IntentionLifecycle::Unresolved;
        let input = input(vec![off, on], None);
        let decision = composed(
            &input,
            &[
                ("i-off", heads(Relation::Conflict, 0.95, 0.0)),
                ("i-on", heads(Relation::Consistent, 0.9, 0.0)),
            ],
        );
        assert_eq!(decision.intervention, Intervention::Clarify);
        assert_eq!(decision.supporting_intentions, vec!["i-off", "i-on"]);
        assert_eq!(decision.holding_intentions, vec!["i-off"]);
    }

    #[test]
    fn a_choice_cites_the_explicit_statement_on_the_other_side() {
        let mut save = intention("i-save", AssertionStatus::DirectOwnerStatement);
        let mut healthy = intention("i-healthy", AssertionStatus::DirectOwnerStatement);
        save.kind = IntentionKind::Goal;
        healthy.kind = IntentionKind::Goal;
        let input = input(vec![save, healthy], None);
        let mut over_budget = heads(Relation::Conflict, 0.9, 0.0);
        over_budget.firm_limit = 0.1;
        let decision = composed(
            &input,
            &[
                ("i-save", over_budget),
                ("i-healthy", heads(Relation::Consistent, 0.9, 0.0)),
            ],
        );
        assert_eq!(decision.continuation, Continuation::Wait);
        assert_eq!(decision.supporting_intentions, vec!["i-save", "i-healthy"]);
        assert_eq!(decision.holding_intentions, vec!["i-save"]);
    }

    #[test]
    fn a_reply_or_a_local_step_earns_at_most_a_mention() {
        let mut reply = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        reply.action = None;
        let decision = composed(&reply, &[("i1", heads(Relation::Conflict, 0.95, 0.0))]);
        assert_eq!(
            (decision.intervention, decision.continuation),
            (Intervention::Nudge, Continuation::Proceed)
        );
        let mut local = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        if let Some(action) = local.action.as_mut() {
            action.outward_class = None;
            action.consequence_class = "private_local".to_string();
        }
        let decision = composed(&local, &[("i1", heads(Relation::Conflict, 0.95, 0.0))]);
        assert_eq!(decision.continuation, Continuation::Proceed);
        // The owner cancelling still cancels.
        let decision = composed(&reply, &[("i1", heads(Relation::Consistent, 0.9, 0.95))]);
        assert_eq!(decision.continuation, Continuation::Cancel);
    }

    #[test]
    fn a_rule_about_outsiders_holds_whoever_the_act_is_for() {
        // "No door codes for anyone outside the household", a code for the
        // plumber: the model reads the subject as someone else, but the
        // relation decides.
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let mut outsiders = heads(Relation::Conflict, 0.9, 0.0);
        outsiders.same_subject = 0.2;
        assert_eq!(composed(&input, &[("i1", outsiders)]).intervention, Intervention::Confirm);
    }

    fn torn(firm_limit: f64, owner_specified: f64) -> IntentionHeads {
        let mut distribution = BTreeMap::new();
        distribution.insert(Relation::Conflict, 0.45);
        distribution.insert(Relation::NotApplicable, 0.43);
        distribution.insert(Relation::Consistent, 0.12);
        IntentionHeads {
            relation: distribution,
            owner_cancels: 0.0,
            owner_specified,
            firm_limit,
            same_subject: 1.0,
            muted: 0.0,
            owner_decides: 0.0,
        }
    }

    #[test]
    fn a_torn_answer_on_a_soft_goal_is_mentioned_and_proceeds() {
        let explicit = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let decision = composed(&explicit, &[("i1", torn(0.1, 0.1))]);
        assert_eq!(
            (decision.assessment, decision.intervention, decision.continuation),
            (Assessment::Unclear, Intervention::Nudge, Continuation::Proceed)
        );
        let inferred = input(vec![intention("i1", AssertionStatus::Inferred)], None);
        let decision = composed(&inferred, &[("i1", torn(0.9, 0.1))]);
        assert_eq!(decision.continuation, Continuation::Proceed);
    }

    #[test]
    fn a_torn_answer_yields_to_the_owners_current_choice_unless_the_rule_is_firm() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        assert_eq!(
            composed(&input, &[("i1", torn(0.2, 0.9))]).assessment,
            Assessment::Aligned
        );
        let decision = composed(&input, &[("i1", torn(0.9, 0.9))]);
        assert_eq!(
            (decision.intervention, decision.continuation),
            (Intervention::Clarify, Continuation::Wait)
        );
    }

    #[test]
    fn the_projection_names_every_intention_by_its_host_id() {
        let input = input(
            vec![
                intention("i1", AssertionStatus::DirectOwnerStatement),
                intention("i2", AssertionStatus::Inferred),
            ],
            None,
        );
        let (context, items) = project(&input);
        assert_eq!(context["owner_turn"], "order dinner");
        assert_eq!(
            items.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["i1", "i2"]
        );
        assert_eq!(items[0].1["statement"], "statement i1");
        // Evidence force is the host's, never shown to the model.
        assert!(items[0].1.get("assertion_status").is_none());
    }

    #[test]
    fn malformed_answers_and_bars_decide_nothing() {
        let input = input(vec![intention("i1", AssertionStatus::DirectOwnerStatement)], None);
        let unresolved = |heads: IntentionHeads| compose(&input, &answers(&[("i1", heads)]), &at(0.5));
        let invalid = Err(ComposeUnresolved::InvalidAnswer("i1".to_string()));
        let mut nan = heads(Relation::Consistent, 0.9, 0.0);
        nan.firm_limit = f64::NAN;
        assert_eq!(unresolved(nan), invalid);
        let mut empty = heads(Relation::Consistent, 0.9, 0.0);
        empty.relation.clear();
        assert_eq!(unresolved(empty), invalid);
        let mut doubled = heads(Relation::Consistent, 0.9, 0.0);
        doubled.relation.insert(Relation::Conflict, 0.9);
        assert_eq!(unresolved(doubled), invalid);
        let mut negative = heads(Relation::Consistent, 0.9, 0.0);
        negative.owner_specified = -0.1;
        assert_eq!(unresolved(negative), invalid);
        let bars = ComposerBars {
            threshold: 1.0,
            ..ComposerBars::default()
        };
        assert!(matches!(
            compose(&input, &answers(&[("i1", heads(Relation::Consistent, 0.9, 0.0))]), &bars),
            Err(ComposeUnresolved::InvalidBars(_))
        ));
    }

    #[test]
    fn a_tie_under_an_advisory_policy_is_only_mentioned() {
        let input = input(
            vec![intention("i1", AssertionStatus::DirectOwnerStatement)],
            Some(InterventionPolicy::Advisory),
        );
        let decision = composed(&input, &[("i1", torn(0.9, 0.1))]);
        assert_eq!(
            (decision.intervention, decision.continuation),
            (Intervention::Nudge, Continuation::Proceed)
        );
    }

    const STATUSES: [AssertionStatus; 5] = [
        AssertionStatus::DirectOwnerStatement,
        AssertionStatus::Confirmed,
        AssertionStatus::Inferred,
        AssertionStatus::InferredUntrusted,
        AssertionStatus::UnknownProvenance,
    ];
    const KINDS: [IntentionKind; 3] = [
        IntentionKind::Taste,
        IntentionKind::Goal,
        IntentionKind::StandingInstruction,
    ];
    const LIFECYCLES: [IntentionLifecycle; 3] = [
        IntentionLifecycle::Active,
        IntentionLifecycle::PendingReview,
        IntentionLifecycle::Unresolved,
    ];
    const POLICIES: [Option<InterventionPolicy>; 4] = [
        None,
        Some(InterventionPolicy::Advisory),
        Some(InterventionPolicy::ChoiceForMaterial),
        Some(InterventionPolicy::ConfirmBeforeException),
    ];

    /// Shapes of the relation answer: each option confident, the near-ties
    /// the bars decide, and no opinion at all.
    fn relation_shapes() -> Vec<BTreeMap<Relation, f64>> {
        use Relation::*;
        let shape = |pairs: &[(Relation, f64)]| pairs.iter().copied().collect::<BTreeMap<_, _>>();
        vec![
            shape(&[(NotApplicable, 0.9), (Conflict, 0.1)]),
            shape(&[(Consistent, 0.9), (Conflict, 0.1)]),
            shape(&[(Conflict, 0.9), (Consistent, 0.1)]),
            shape(&[(Unclear, 0.9), (Consistent, 0.1)]),
            shape(&[(AcknowledgedException, 0.9), (Conflict, 0.1)]),
            shape(&[(Conflict, 0.45), (NotApplicable, 0.43), (Consistent, 0.12)]),
            shape(&[(Consistent, 0.55), (Conflict, 0.45)]),
            shape(&[(Conflict, 0.48), (Unclear, 0.48), (Consistent, 0.04)]),
            shape(&[
                (NotApplicable, 0.2),
                (Consistent, 0.2),
                (Conflict, 0.2),
                (Unclear, 0.2),
                (AcknowledgedException, 0.2),
            ]),
        ]
    }

    fn side_grid() -> Vec<(f64, f64, f64, f64)> {
        let mut grid = Vec::new();
        for cancels in [0.02, 0.5, 0.97] {
            for specified in [0.1, 0.9] {
                for firm in [0.1, 0.9] {
                    for subject in [0.2, 0.9] {
                        grid.push((cancels, specified, firm, subject));
                    }
                }
            }
        }
        grid
    }

    /// The action shapes: outward, a private local step, and none at all.
    fn with_action(mut input: AlignmentInput, shape: usize) -> AlignmentInput {
        match shape {
            1 => {
                if let Some(action) = input.action.as_mut() {
                    action.outward_class = None;
                }
            },
            2 => input.action = None,
            _ => {},
        }
        input
    }

    fn shaped(id: &str, status: AssertionStatus, kind: IntentionKind, lifecycle: IntentionLifecycle) -> Intention {
        let mut intention = intention(id, status);
        intention.kind = kind;
        intention.lifecycle = lifecycle;
        intention
    }

    /// Whatever the answers, a composed decision is one the host validator
    /// accepts unchanged, and it keeps every force rule. Checks one
    /// composition; returns a description of the first broken guarantee.
    fn broken_guarantee(input: &AlignmentInput, answers: &BTreeMap<String, IntentionHeads>) -> Option<String> {
        let bars = ComposerBars::default();
        let decision = match compose(input, answers, &bars) {
            Ok(decision) => decision,
            Err(ComposeUnresolved::Uncertain { question, .. }) if question == CANCEL_QUESTION => {
                return None;
            },
            Err(other) => return Some(format!("unexpected unresolved: {other}")),
        };
        match validate_decision(input, decision.clone()) {
            Ok(validated) if validated.downgrade.is_none() => {},
            Ok(validated) => return Some(format!("needed lowering: {:?}", validated.downgrade)),
            Err(rejection) => return Some(format!("rejected: {rejection:?}")),
        }
        let held_by = |id: &String| input.intentions.iter().find(|intention| &intention.id == id);
        if decision.intervention == Intervention::Confirm
            && !decision
                .holding_intentions
                .iter()
                .filter_map(held_by)
                .all(|intention| intention.assertion_status.may_hold())
        {
            return Some("a confirm rests on evidence that may not hold".to_string());
        }
        if decision.continuation == Continuation::Wait
            && decision
                .holding_intentions
                .iter()
                .filter_map(held_by)
                .any(|intention| !intention.assertion_status.may_offer_choice())
        {
            return Some("a wait rests on untrusted or unknown evidence".to_string());
        }
        None
    }

    #[test]
    fn every_single_intention_combination_composes_within_the_force_rules() {
        let shapes = relation_shapes();
        let grid = side_grid();
        let bars = ComposerBars::default();
        let mut checked = 0usize;
        for status in STATUSES {
            for kind in KINDS {
                for lifecycle in LIFECYCLES {
                    for policy in POLICIES {
                        for action in 0..3 {
                            let base = with_action(input(vec![shaped("i1", status, kind, lifecycle)], policy), action);
                            for relation in &shapes {
                                for (cancels, specified, firm, subject) in &grid {
                                    let heads = IntentionHeads {
                                        relation: relation.clone(),
                                        owner_cancels: *cancels,
                                        owner_specified: *specified,
                                        firm_limit: *firm,
                                        same_subject: *subject,
                                        muted: 0.0,
                                        owner_decides: 0.0,
                                    };
                                    let answers = answers(&[("i1", heads.clone())]);
                                    let case = format!(
                                        "{status:?}/{kind:?}/{lifecycle:?}/{policy:?}/action {action}/{relation:?}/{cancels},{specified},{firm},{subject}"
                                    );
                                    if let Some(broken) = broken_guarantee(&base, &answers) {
                                        panic!("{case}: {broken}");
                                    }
                                    checked += 1;
                                    let intention = &base.intentions[0];
                                    let conflict = relation.get(&Relation::Conflict).copied().unwrap_or(0.0);
                                    let firm_rule = *firm > bars.soft || kind == IntentionKind::StandingInstruction;
                                    // A confident conflict with a firm explicit rule on an
                                    // outward act never proceeds, unless the owner asked to
                                    // be advised only or cancelled the act.
                                    if status.may_hold()
                                        && firm_rule
                                        && conflict >= 0.9
                                        && action == 0
                                        && *cancels < 0.05
                                        && policy != Some(InterventionPolicy::Advisory)
                                    {
                                        let decision = compose(&base, &answers, &bars).expect("composes");
                                        assert_eq!(decision.continuation, Continuation::Wait, "{case}");
                                    }
                                    // A tie never intervenes harder than a settled `unclear`.
                                    if settled(&heads, intention, &bars).is_none() {
                                        let tie = capped(&base, tie_effect(&base, intention, &heads, &bars));
                                        let unclear = effect_of(&base, intention, Relation::Unclear, &heads, &bars);
                                        assert!(tie <= unclear, "{case}: tie {tie:?} above unclear {unclear:?}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 5 * 3 * 3 * 4 * 3 * 9 * 24);
    }

    #[test]
    fn every_pair_of_intentions_composes_within_the_force_rules() {
        let shapes = relation_shapes();
        for first in STATUSES {
            for second in STATUSES {
                for policy in POLICIES {
                    for a in &shapes {
                        for b in &shapes {
                            let input = input(
                                vec![
                                    shaped("i1", first, IntentionKind::StandingInstruction, IntentionLifecycle::Active),
                                    shaped("i2", second, IntentionKind::Goal, IntentionLifecycle::Unresolved),
                                ],
                                policy,
                            );
                            for (firm, muted) in [(0.9, 0.0), (0.1, 0.0), (0.1, 0.9), (0.9, 0.9)] {
                                let heads = |relation: &BTreeMap<Relation, f64>| IntentionHeads {
                                    relation: relation.clone(),
                                    owner_cancels: 0.02,
                                    owner_specified: 0.1,
                                    firm_limit: firm,
                                    same_subject: 0.9,
                                    muted,
                                    owner_decides: 0.0,
                                };
                                let answers = answers(&[("i1", heads(a)), ("i2", heads(b))]);
                                if let Some(broken) = broken_guarantee(&input, &answers) {
                                    panic!("{first:?}+{second:?}/{policy:?}/{a:?}+{b:?}/firm {firm}, muted {muted}: {broken}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn conflict_heads(firm_limit: f64, muted: f64) -> IntentionHeads {
        let mut heads = heads(Relation::Conflict, 0.9, 0.0);
        heads.firm_limit = firm_limit;
        heads.muted = muted;
        heads
    }

    #[test]
    fn the_owners_own_decision_satisfies_a_rule_that_reserves_it() {
        // "Don't agree to lend the car - ask me first"; the owner: "Tell
        // Petrov yes" (owner ruling 2026-10-11).
        let rule = shaped("lend", AssertionStatus::DirectOwnerStatement, IntentionKind::StandingInstruction, IntentionLifecycle::Active);
        let lend = input(vec![rule.clone()], None);
        let decided = |p: f64| IntentionHeads { owner_decides: p, ..heads(Relation::Conflict, 0.95, 0.0) };
        let decision = composed(&lend, &[("lend", decided(0.85))]);
        assert_eq!(decision.assessment, Assessment::AcknowledgedException);
        assert_eq!(decision.continuation, Continuation::Proceed);
        // Below the bar (Jev's misfires sat at 0.50-0.51) the rule asks.
        let decision = composed(&lend, &[("lend", decided(0.65))]);
        assert_eq!(decision.intervention, Intervention::Confirm);
        // Only the current owner turn can make the decision.
        let mut no_turn = lend.clone();
        no_turn.request.owner_spans.clear();
        let decision = compose(&no_turn, &answers(&[("lend", decided(0.95))]), &at(0.7)).expect("composes");
        assert_eq!(decision.intervention, Intervention::Confirm);
        // Never for anything but a standing instruction.
        let goal = input(
            vec![shaped("goal", AssertionStatus::DirectOwnerStatement, IntentionKind::Goal, IntentionLifecycle::Active)],
            None,
        );
        let decision = composed(&goal, &[("goal", decided(0.95))]);
        assert_ne!(decision.assessment, Assessment::AcknowledgedException);
    }

    #[test]
    fn an_owner_instruction_mutes_a_goal_for_this_act() {
        // "Don't bring up the deposit goal for things under $300", a $240 buy.
        let input = input(
            vec![
                shaped("no-nag", AssertionStatus::DirectOwnerStatement, IntentionKind::StandingInstruction, IntentionLifecycle::Active),
                shaped("deposit", AssertionStatus::DirectOwnerStatement, IntentionKind::Goal, IntentionLifecycle::Active),
            ],
            None,
        );
        let decision = composed(
            &input,
            &[
                ("no-nag", heads(Relation::Consistent, 0.95, 0.0)),
                ("deposit", conflict_heads(0.1, 0.9)),
            ],
        );
        assert_eq!(decision.assessment, Assessment::Aligned);
        // Unmuted, the goal offers a choice.
        let decision = composed(
            &input,
            &[
                ("no-nag", heads(Relation::Consistent, 0.95, 0.0)),
                ("deposit", conflict_heads(0.1, 0.1)),
            ],
        );
        assert_eq!(decision.continuation, Continuation::Wait);
    }

    #[test]
    fn a_mute_never_silences_a_firm_rule_and_needs_the_owners_instruction() {
        let owner_rule = shaped("no-nag", AssertionStatus::DirectOwnerStatement, IntentionKind::StandingInstruction, IntentionLifecycle::Active);
        let firm = input(
            vec![
                owner_rule.clone(),
                shaped("codes", AssertionStatus::DirectOwnerStatement, IntentionKind::StandingInstruction, IntentionLifecycle::Active),
            ],
            None,
        );
        let decision = composed(
            &firm,
            &[("no-nag", heads(Relation::Consistent, 0.95, 0.0)), ("codes", conflict_heads(0.9, 0.95))],
        );
        assert_eq!(decision.intervention, Intervention::Confirm, "a firm rule is never muted");
        // Only an inferred "instruction" in the input: nothing can mute.
        let inferred_rule = input(
            vec![
                shaped("no-nag", AssertionStatus::Inferred, IntentionKind::StandingInstruction, IntentionLifecycle::Active),
                shaped("deposit", AssertionStatus::DirectOwnerStatement, IntentionKind::Goal, IntentionLifecycle::Active),
            ],
            None,
        );
        let decision = composed(
            &inferred_rule,
            &[("no-nag", heads(Relation::Consistent, 0.95, 0.0)), ("deposit", conflict_heads(0.1, 0.95))],
        );
        assert_eq!(decision.continuation, Continuation::Wait, "an inference cannot mute");
    }

    #[test]
    fn the_projection_shows_the_other_statements_without_their_force() {
        let input = input(
            vec![
                shaped("a", AssertionStatus::DirectOwnerStatement, IntentionKind::StandingInstruction, IntentionLifecycle::Active),
                shaped("b", AssertionStatus::Inferred, IntentionKind::Goal, IntentionLifecycle::Active),
            ],
            None,
        );
        let (context, _) = project(&input);
        assert_eq!(context["intentions"].as_array().unwrap().len(), 2);
        assert!(!context.to_string().contains("inferred"), "{context}");
        assert!(!context.to_string().contains("direct_owner_statement"), "{context}");
    }

    #[test]
    fn a_lone_intention_is_not_repeated_in_context() {
        let input = input(
            vec![shaped("a", AssertionStatus::DirectOwnerStatement, IntentionKind::StandingInstruction, IntentionLifecycle::Active)],
            None,
        );
        let (context, items) = project(&input);
        assert!(context.get("intentions").is_none(), "{context}");
        assert_eq!(items.len(), 1);
    }
}
