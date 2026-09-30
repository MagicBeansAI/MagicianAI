//! Whether an act is inside an envelope — plan §4, §5 and §4A.
//!
//! Pure: it reads an envelope's folded state and a set of facts, and returns a
//! verdict. No I/O, no clock of its own, no store handle. That is deliberate —
//! a decision function that can read is a decision function that can be made to
//! read the wrong thing, and this one is the whole security boundary.
//!
//! # Order matters, and it is the cheap-and-absolute checks first
//!
//! Bindability, class and revocation are absolute: no envelope configuration can
//! override them. Limits and predicates come after, because they are the ones an
//! owner tunes. A reader should be able to see the un-overridable rules without
//! reading the whole function.

use crate::magician_v2::agents::ConsequenceClass;

use super::types::{
    ActFacts, BoundaryPredicate, EnvelopeDecision, EnvelopeKind, EnvelopeState, NotCoveredReason,
};

/// Decide whether one act is covered by one envelope.
///
/// Returns `Covered` only when **every** gate passes. There is no partial
/// coverage and no "covered with a warning": §6's inversion means the absence of
/// a clear yes is a no.
pub fn resolve(state: &EnvelopeState, facts: &ActFacts<'_>) -> EnvelopeDecision {
    let envelope = &state.envelope;

    // ── Absolute: nothing in a grant can override these ─────────────────────

    // §4A. An act with an escape hatch cannot be bound to a decision, because
    // the tokens we did not inspect can still change what it does. Authorising
    // it would authorise a description of the act rather than the act.
    //
    // This is checked FIRST, before class or limits, so that no combination of
    // envelope settings can produce a `Covered` verdict for an unbindable act.
    // It is also why envelopes are not decoration before restricted outward
    // actions exist: today every raw sending skill lands here, and when the
    // restricted forms arrive they become bindable and start being covered.
    if !facts.effective.is_bindable() {
        return not_covered(NotCoveredReason::ActionNotBindable {
            escape_hatches: facts.effective.escape_hatches.clone(),
        });
    }

    // An act with no stable id cannot be debited idempotently, so authorising it
    // would let one debit slot stand for an unbounded number of acts.
    if facts.act_ref.trim().is_empty() {
        return not_covered(NotCoveredReason::UnidentifiedAct);
    }

    // Commitment is never covered, by any kind of envelope, with no exception
    // anywhere in the plan. Asserted through the taxonomy rather than re-decided
    // here, so it cannot be true in one place and false in another.
    if !envelope.kind.may_cover(facts.consequence_class) {
        return not_covered(NotCoveredReason::ClassNotCoverable {
            class: facts.consequence_class,
            kind: envelope.kind.label().to_string(),
        });
    }

    // Coverable in principle is not the same as granted. An owner who granted
    // bounded communication did not thereby grant disclosure.
    if !envelope.covers.contains(&facts.consequence_class) {
        return not_covered(NotCoveredReason::ClassNotGranted {
            class: facts.consequence_class,
        });
    }

    if envelope.is_revoked() {
        return not_covered(NotCoveredReason::Revoked);
    }

    if envelope.is_expired(facts.now) {
        return not_covered(NotCoveredReason::Expired);
    }

    // ── Limits. Each degrades to asking, never to proceeding ────────────────

    if let Some(reason) = limit_reached(state, facts) {
        return not_covered(reason);
    }

    // ── The boundary ────────────────────────────────────────────────────────

    let mut matched_predicates = Vec::with_capacity(envelope.boundary.len() + 1);

    // A batch is closed: its instances ARE its boundary, so this is checked
    // whether or not the grant listed any predicate.
    if let EnvelopeKind::ReviewedBatch { instances } = &envelope.kind {
        for recipient in &facts.effective.recipients {
            if !instances
                .iter()
                .any(|listed| &listed.recipient == recipient)
            {
                return not_covered(NotCoveredReason::NotInReviewedBatch {
                    recipient: recipient.clone(),
                });
            }
        }
        matched_predicates.push("reviewed_batch_instance".to_string());
    }

    for predicate in &envelope.boundary {
        if let Err(detail) = evaluate(predicate, facts) {
            return not_covered(NotCoveredReason::PredicateFailed {
                predicate: predicate.name().to_string(),
                detail,
            });
        }
        matched_predicates.push(predicate.name().to_string());
    }

    EnvelopeDecision::Covered {
        envelope_id: envelope.envelope_id.clone(),
        outcome: envelope.outcome.clone(),
        matched_predicates,
    }
}

/// The first limit this act would exceed, if any.
///
/// Counted against what is **already** debited plus this act, so the cap is a
/// ceiling on the total rather than on the history — an envelope for ten acts
/// authorises ten, not eleven.
fn limit_reached(state: &EnvelopeState, facts: &ActFacts<'_>) -> Option<NotCoveredReason> {
    let limits = &state.envelope.limits;

    // A retry of an act already debited must not be refused for a cap its own
    // earlier attempt filled. Without this, an idempotent resend of the tenth
    // act under a ten-act envelope would ask.
    let replaying = state.already_debited(facts.act_ref);
    let prospective_acts = if replaying {
        state.acts_used()
    } else {
        state.acts_used() + 1
    };

    if let Some(max_acts) = limits.max_acts {
        if prospective_acts > max_acts {
            return Some(NotCoveredReason::LimitReached {
                limit: "max_acts".to_string(),
            });
        }
    }

    if let Some(cap) = limits.per_recipient_cap {
        for recipient in &facts.effective.recipients {
            let used = state.acts_for_recipient(recipient);
            let prospective = if replaying { used } else { used + 1 };
            if prospective > cap {
                return Some(NotCoveredReason::LimitReached {
                    limit: "per_recipient_cap".to_string(),
                });
            }
        }
    }

    if let Some(ceiling) = limits.max_value_micros {
        let spent = state.value_used_micros();
        let prospective = if replaying {
            spent
        } else {
            spent.saturating_add(facts.value_micros.unwrap_or(0))
        };
        if prospective > ceiling {
            return Some(NotCoveredReason::LimitReached {
                limit: "max_value_micros".to_string(),
            });
        }
    }

    None
}

/// Evaluate one boundary predicate. `Err(detail)` is a false verdict with its
/// reason attached, so a refusal can be explained.
fn evaluate(predicate: &BoundaryPredicate, facts: &ActFacts<'_>) -> Result<(), String> {
    match predicate {
        BoundaryPredicate::RecipientInEngagement { engagement_id } => {
            // The act must be ON the engagement the envelope named. Without
            // this, an envelope scoped to one engagement would be satisfied by
            // any act whose recipients happened to appear in that engagement's
            // identity list.
            if facts.engagement_id != Some(engagement_id.as_str()) {
                return Err(format!(
                    "act is on engagement {:?}, envelope binds {engagement_id}",
                    facts.engagement_id
                ));
            }
            // An act that names nobody cannot satisfy a predicate about who it
            // reaches. Treating "no recipients" as vacuously inside the
            // engagement is how an unaddressed act slips through a boundary
            // built entirely from recipients.
            if facts.effective.recipients.is_empty() {
                return Err("act names no recipients".to_string());
            }
            for recipient in &facts.effective.recipients {
                if !facts.engagement_identities.iter().any(|id| id == recipient) {
                    return Err(format!("{recipient} is not an identity on {engagement_id}"));
                }
            }
            Ok(())
        },
        BoundaryPredicate::CapabilityInSet { capabilities } => {
            let capability = facts.effective.capability.as_str();
            if capabilities.iter().any(|allowed| allowed == capability) {
                Ok(())
            } else {
                Err(format!("{capability} is not in the granted set"))
            }
        },
        BoundaryPredicate::NoAttachmentOutsideLedger => match facts.attachments_outside_ledger {
            Some(0) => Ok(()),
            Some(outside) => Err(format!("{outside} attachment(s) outside the asset ledger")),
            // Unknown is not zero. A caller with no ledger to consult would
            // otherwise satisfy this by having nothing to report, which is the
            // vacuous-truth shape rather than a check.
            None => Err("attachment provenance is unknown to the caller".to_string()),
        },
    }
}

fn not_covered(reason: NotCoveredReason) -> EnvelopeDecision {
    EnvelopeDecision::NotCovered { reason }
}

/// Resolve against every candidate envelope, returning the first that covers.
///
/// When none covers, the reason returned is the **most specific** one seen
/// rather than the first, because "no envelope" is what an owner is told when
/// their envelope exists and a cap is spent, and that answer sends them looking
/// in the wrong place.
pub fn resolve_any(states: &[EnvelopeState], facts: &ActFacts<'_>) -> EnvelopeDecision {
    // Bindability is a property of the ACT, not of any envelope, so it is
    // answered once rather than rediscovered per candidate.
    //
    // Two reasons this must be here and not left to the per-envelope pass.
    // Correctness: an unbindable act next to an expired envelope would otherwise
    // report `Expired` — the more "specific" reason — and send the owner off to
    // renew an envelope that still would not cover it. Cost: it also stops N
    // full resolutions for an answer that could never have been yes.
    if !facts.effective.is_bindable() {
        return not_covered(NotCoveredReason::ActionNotBindable {
            escape_hatches: facts.effective.escape_hatches.clone(),
        });
    }

    // Deterministic order, and a useful one. Whichever envelope covers FIRST is
    // the one debited, so iterating in storage order made "which of my envelopes
    // paid for this" depend on the order they happened to be granted in.
    //
    // Soonest expiry first spends the authority that is about to lapse, so
    // headroom is not stranded in an envelope that then expires unused. The id
    // breaks ties, so the answer is stable across runs rather than merely
    // deterministic within one.
    let mut order: Vec<&EnvelopeState> = states.iter().collect();
    order.sort_by(|left, right| {
        expiry_key(left)
            .cmp(&expiry_key(right))
            .then_with(|| left.envelope.envelope_id.cmp(&right.envelope.envelope_id))
    });

    let mut best: Option<NotCoveredReason> = None;

    for state in order {
        match resolve(state, facts) {
            covered @ EnvelopeDecision::Covered { .. } => return covered,
            EnvelopeDecision::NotCovered { reason } => {
                if best
                    .as_ref()
                    .is_none_or(|held| specificity(&reason) > specificity(held))
                {
                    best = Some(reason);
                }
            },
        }
    }

    not_covered(best.unwrap_or(NotCoveredReason::NoEnvelope))
}

/// Sort key placing soonest-expiring envelopes first, with never-expiring ones
/// last — they can always be spent later, so they are the wrong thing to spend
/// while something is about to lapse.
fn expiry_key(state: &EnvelopeState) -> (u8, i64) {
    match state.envelope.limits.expires_at {
        Some(expiry) => (0, expiry.timestamp_millis()),
        None => (1, 0),
    }
}

/// How close a refusal came to being a yes. Higher is more specific, and a more
/// specific reason is the more useful thing to report.
///
/// `ActionNotBindable` ranks ABOVE every envelope-specific reason: it is a fact
/// about the act, so no amount of fixing the envelope changes it, and reporting
/// anything else would point the owner at the wrong thing to fix. `resolve_any`
/// short-circuits on it before this is consulted; the ordering here keeps the
/// two consistent if a caller reaches `resolve` directly.
fn specificity(reason: &NotCoveredReason) -> u8 {
    match reason {
        NotCoveredReason::NoEnvelope => 0,
        NotCoveredReason::NotEnforcing => 1,
        NotCoveredReason::ClassNotCoverable { .. } => 2,
        NotCoveredReason::ClassNotGranted { .. } => 3,
        NotCoveredReason::Revoked => 4,
        NotCoveredReason::Expired => 5,
        NotCoveredReason::NotInReviewedBatch { .. } => 6,
        NotCoveredReason::PredicateFailed { .. } => 7,
        NotCoveredReason::LimitReached { .. } => 8,
        // Both are facts about the ACT rather than the envelope, so no amount of
        // fixing an envelope changes them and reporting anything else points the
        // owner at the wrong thing.
        NotCoveredReason::ActionNotBindable { .. } => 9,
        NotCoveredReason::UnidentifiedAct => 10,
    }
}

/// The class an act must be for a standing envelope to be able to carry it at
/// all. Exposed so a grant surface can explain the rule rather than restate it.
pub const STANDING_COVERABLE: ConsequenceClass = ConsequenceClass::BoundedCommunication;
