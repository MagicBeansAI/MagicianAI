//! Envelopes in front of `requires_approval` — plan phase 5.
//!
//! §11 is explicit that this does **not** replace `requires_approval`:
//! *"envelopes sit in front of it and it remains the fallback for everything
//! uncovered."* So this answers one question — *may this particular prompt be
//! waived?* — and answers "no" for everything it is not certain about.
//!
//! # Why this is the generalisation, and not more wiring at the outward gate
//!
//! §5 of the plan: *"Nothing in §3–§6 is specific to the OPC; it was simply found
//! there first."* Outward acts are where consent-per-act hurts most, but the
//! model is about **any** act an owner is asked to approve. `requires_approval`
//! is the one place every such ask converges, which makes it the only
//! integration point that generalises without being enumerated.
//!
//! # The classification used here is the strict one
//!
//! An act reaching this function is, by definition, one somebody put behind
//! approval. So it is classified with
//! [`consequence_class_for_approval_rule`](crate::magician_v2::agents::consequence_class_for_approval_rule),
//! which never answers `private_local` and fails closed to
//! `commitment_or_transaction` for anything unclassified.
//!
//! That matters more here than anywhere else. This is the layer that actually
//! **removes a prompt**, so a generalisation that quietly waived asks for acts
//! nobody had classified would be the worst possible outcome of "make it
//! generic". Unclassified means commitment means never waived.

use anyhow::Result;

use crate::magician_v2::agents::{consequence_class_for_approval_rule, ConsequenceClass};

use super::gate::{DispatchContext, EnvelopeGate, GateOutcome};
use super::types::{EnvelopeMode, NotCoveredReason};

/// Whether an approval prompt may be skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalWaiver {
    /// Ask the owner, exactly as before envelopes existed.
    ///
    /// `reason` is `None` when nothing was evaluated at all (mode off), and
    /// `Some` when an envelope was consulted and declined — a distinction an
    /// owner surface needs, because the second means "your envelope did not
    /// cover this" and the first means "envelopes are not on".
    Ask { reason: Option<NotCoveredReason> },
    /// The owner already consented to this outcome. Names the envelope and the
    /// predicates that matched, so §7's audit holds for a waived prompt exactly
    /// as it does for an act.
    Waived {
        envelope_id: String,
        outcome: String,
        matched_predicates: Vec<String>,
    },
}

impl ApprovalWaiver {
    pub fn is_waived(&self) -> bool {
        matches!(self, Self::Waived { .. })
    }

    /// A line for the approval log, so a waived prompt is as auditable as an
    /// asked one. A prompt that vanished without a record would be the thing
    /// that makes envelopes untrustworthy.
    pub fn audit_line(&self) -> String {
        match self {
            Self::Ask { reason: None } => "asked (envelopes off)".to_string(),
            Self::Ask {
                reason: Some(reason),
            } => format!("asked ({})", super::gate::reason_label(reason)),
            Self::Waived {
                envelope_id,
                outcome,
                matched_predicates,
            } => format!(
                "waived by {envelope_id} ({outcome:?}) on {}",
                matched_predicates.join("+")
            ),
        }
    }
}

/// Decide whether an approval that *would* be asked may be waived.
///
/// The caller has already determined that this act requires approval; this only
/// says whether a standing consent covers it.
///
/// # This CONSUMES the envelope
///
/// A waiver is the act's authorisation, so being waived debits the envelope —
/// the same commit the dispatch gate performs, for the same reason (a crash
/// after acting and before debiting costs the cap entirely).
///
/// So this must be called **once, at the point of deciding**, not speculatively.
/// A surface asking "would this need a prompt?" to decide what to render must
/// use [`preview_approval_waiver`], or every render spends an act.
///
/// # Two guarantees a caller may rely on
///
/// - **`Shadow` never waives.** It resolves and reports so the decision can be
///   compared against what the owner actually did, and returns `Ask` regardless.
/// - **A commitment is never waived**, by any envelope, in any mode. Guaranteed
///   twice over: the class is computed with the approval-rule classifier, which
///   fails closed to commitment, and the resolver refuses commitment for every
///   envelope kind.
pub fn resolve_approval_waiver(
    gate: &EnvelopeGate,
    mode: EnvelopeMode,
    capability: &str,
    action: &str,
    ctx_without_class: ApprovalContext<'_>,
) -> Result<ApprovalWaiver> {
    let consequence_class = consequence_class_for_approval_rule(capability, action);

    // Belt and braces. The resolver enforces this for every envelope kind, but
    // this is the layer that removes a prompt, and a rule with no exception
    // anywhere in the plan should be visible at the layer where breaking it
    // would cost the most.
    if matches!(consequence_class, ConsequenceClass::CommitmentOrTransaction) {
        return Ok(ApprovalWaiver::Ask {
            reason: Some(NotCoveredReason::ClassNotCoverable {
                class: consequence_class,
                kind: "any".to_string(),
            }),
        });
    }

    let ctx = DispatchContext {
        act_ref: ctx_without_class.act_ref,
        effective: ctx_without_class.effective,
        consequence_class,
        envelope_scope: ctx_without_class.envelope_scope,
        engagement_id: ctx_without_class.engagement_id,
        engagement_identities: ctx_without_class.engagement_identities,
        attachments_outside_ledger: ctx_without_class.attachments_outside_ledger,
        value_micros: ctx_without_class.value_micros,
        now: ctx_without_class.now,
    };

    match gate.evaluate(mode, &ctx)? {
        GateOutcome::NotEvaluated => Ok(ApprovalWaiver::Ask { reason: None }),
        GateOutcome::Shadow { decision } => Ok(ApprovalWaiver::Ask {
            reason: match decision {
                super::types::EnvelopeDecision::NotCovered { reason } => Some(reason),
                // Covered in shadow still asks. The reason recorded is that the
                // mode is not enforcing, which is the truthful answer to "why
                // was I asked" — not the coverage that would have applied.
                super::types::EnvelopeDecision::Covered { .. } => {
                    Some(NotCoveredReason::NotEnforcing)
                },
            },
        }),
        GateOutcome::Refused { reason } => Ok(ApprovalWaiver::Ask {
            reason: Some(reason),
        }),
        GateOutcome::Authorised {
            envelope_id,
            outcome,
            matched_predicates,
        } => Ok(ApprovalWaiver::Waived {
            envelope_id,
            outcome,
            matched_predicates,
        }),
    }
}

/// Whether an approval *would* be waived, without consuming anything.
///
/// For surfaces and dry runs: rendering a screen, previewing what a campaign
/// will ask about, or logging what the current envelopes would cover. It
/// resolves through shadow semantics, so it reads and never debits.
///
/// **The answer is advisory.** Between a preview and the real decision an
/// envelope can expire, be revoked or be spent by another act, so a preview that
/// said "waived" does not promise the commit will. Treating it as a promise is
/// the mistake this doc exists to prevent.
pub fn preview_approval_waiver(
    gate: &EnvelopeGate,
    mode: EnvelopeMode,
    capability: &str,
    action: &str,
    ctx: ApprovalContext<'_>,
) -> Result<ApprovalWaiver> {
    // `Off` stays `Off`: a preview must not resolve when the feature is not on,
    // or a surface would show coverage that could never apply.
    let preview_mode = match mode {
        EnvelopeMode::Off => EnvelopeMode::Off,
        EnvelopeMode::Shadow | EnvelopeMode::Enforcing => EnvelopeMode::Shadow,
    };
    resolve_approval_waiver(gate, preview_mode, capability, action, ctx)
}

/// Everything [`resolve_approval_waiver`] needs except the consequence class,
/// which it derives itself so a caller cannot supply a softer one.
///
/// That omission is the point. A caller passing its own class could hand in
/// `bounded_communication` for a payment and waive the prompt, which is exactly
/// the shape of every authority bug worth preventing.
#[derive(Debug, Clone)]
pub struct ApprovalContext<'a> {
    pub act_ref: &'a str,
    pub effective: &'a crate::magician_v2::execution::EffectiveAction,
    pub envelope_scope: Option<super::types::EnvelopeScope>,
    pub engagement_id: Option<&'a str>,
    pub engagement_identities: &'a [String],
    pub attachments_outside_ledger: Option<usize>,
    pub value_micros: Option<u64>,
    pub now: chrono::DateTime<chrono::Utc>,
}
