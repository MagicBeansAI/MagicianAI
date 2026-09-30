//! The envelope, its boundary, and the facts a decision may be made from.
//!
//! Plan §4, §5 and §6 of `docs/plans/2026-08-07-opc-approval-envelopes.md`.
//!
//! Structurally this is Resource Authority with a different commodity space —
//! commodity → outcome class, budget row → envelope, token ledger → consumption
//! ledger — so it follows RA's shapes, including `DateTime<Utc>` for every
//! instant. **With one deliberate inversion:** RA fails OPEN, because capping a
//! declared commodity must not block every other tool a user explicitly invoked.
//! Envelopes fail CLOSED, because these acts are autonomous, self-initiated and
//! outward: nobody asked for them, so a missing envelope must read as silence
//! rather than as consent.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::ConsequenceClass;
use crate::magician_v2::execution::EffectiveAction;

/// What an envelope is scoped to. Never "everything": an envelope with no
/// narrower home than the whole workspace is the blanket yes §9 exists to stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum EnvelopeScope {
    Goal(String),
    Program(String),
    Engagement(String),
}

/// An act performed inside a work context is scoped to that work's envelopes.
///
/// This is the bridge the shadow log named as its own finding: *"no outward act
/// is currently scoped to anything an envelope could be granted against"*. A
/// caller that knows which work it is serving (`work_context::WorkContextKind`)
/// derives the envelope scope from it rather than inventing a second notion of
/// "the work this belongs to".
///
/// The impl lives HERE, not in `work_context`, so the capability-resolution
/// module stays dependency-free: envelopes consume the work context, never the
/// reverse.
impl From<&crate::magician_v2::work_context::WorkContextKind> for EnvelopeScope {
    fn from(kind: &crate::magician_v2::work_context::WorkContextKind) -> Self {
        use crate::magician_v2::work_context::WorkContextKind;
        match kind {
            WorkContextKind::Program(id) => Self::Program(id.clone()),
            WorkContextKind::Engagement(id) => Self::Engagement(id.clone()),
        }
    }
}

impl EnvelopeScope {
    pub fn as_key(&self) -> String {
        match self {
            Self::Goal(id) => format!("goal:{id}"),
            Self::Program(id) => format!("program:{id}"),
            Self::Engagement(id) => format!("engagement:{id}"),
        }
    }
}

/// One instance the owner actually reviewed, for a batch.
///
/// §3.1: a batch goes wider than a standing envelope *because the owner saw the
/// instances*. It is exhausted by its own list and nothing can be added to it,
/// which is the only reason it may carry disclosure and submission at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchInstance {
    /// Canonicalised at grant time, because the resolver compares against a
    /// canonicalised effective recipient. Two spellings of one target would
    /// otherwise read as "not on the list".
    pub recipient: String,
    /// The content the owner reviewed, when the batch pinned one.
    pub content_ref: Option<String>,
}

/// Standing consent to a class of future acts, or consent to an enumerated list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvelopeKind {
    /// Consent to acts the owner has not seen. Bounded communication only.
    Standing,
    /// Consent to instances the owner reviewed. Wider in class, narrower in
    /// reach — and closed: `instances` is the whole of it.
    ReviewedBatch { instances: Vec<BatchInstance> },
}

impl EnvelopeKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::ReviewedBatch { .. } => "reviewed_batch",
        }
    }

    /// Whether this kind may carry `class` at all, before any predicate runs.
    ///
    /// Delegates to the class rather than re-deciding: the taxonomy lives in one
    /// place, so "commitment is never covered" cannot be true here and false
    /// there.
    pub fn may_cover(&self, class: ConsequenceClass) -> bool {
        match self {
            Self::Standing => class.standing_envelope_may_cover(),
            Self::ReviewedBatch { .. } => class.reviewed_batch_may_cover(),
        }
    }
}

/// Hard caps. Every one of these degrades to asking rather than to proceeding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeLimits {
    pub max_acts: Option<u32>,
    pub per_recipient_cap: Option<u32>,
    pub max_value_micros: Option<u64>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// A boundary condition on the act's **shape**.
///
/// §5: only facts the agent does not author. Neither the agent's account of
/// which outcome it is performing nor an LLM's classification of the act is an
/// input here — both are the same bug one layer apart, and the second is the
/// injection surface moved rather than removed.
///
/// Counts, windows and value ceilings are [`EnvelopeLimits`] instead of
/// predicates: they are checked on every resolution regardless of what the
/// grant listed, so expressing them twice could only ever disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "predicate", rename_all = "snake_case")]
pub enum BoundaryPredicate {
    /// Every recipient is already an identity on the engagement.
    ///
    /// The identities come from the engagement store via [`ActFacts`], never
    /// from the act — an agent that could name its own counterparty list would
    /// satisfy this by writing it.
    RecipientInEngagement { engagement_id: String },
    /// The capability is one of a named set.
    CapabilityInSet { capabilities: Vec<String> },
    /// No attachment outside the asset ledger.
    NoAttachmentOutsideLedger,
}

impl BoundaryPredicate {
    /// Stable name for the audit record (§7). A covered act names the envelope
    /// **and** which predicates matched, so "why did it send that" has an answer
    /// without reconstruction.
    pub fn name(&self) -> &'static str {
        match self {
            Self::RecipientInEngagement { .. } => "recipient_in_engagement",
            Self::CapabilityInSet { .. } => "capability_in_set",
            Self::NoAttachmentOutsideLedger => "no_attachment_outside_ledger",
        }
    }
}

/// Pre-authorisation for a class of future acts, bounded and revocable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalEnvelope {
    pub envelope_id: String,
    pub scope: EnvelopeScope,
    /// What the owner believes they consented to, in their words. Human-readable
    /// and deliberately **not** an input to any decision — it is for the owner
    /// surface and the audit trail.
    pub outcome: String,
    pub kind: EnvelopeKind,
    pub covers: Vec<ConsequenceClass>,
    pub limits: EnvelopeLimits,
    pub boundary: Vec<BoundaryPredicate>,
    pub granted_by: String,
    pub granted_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApprovalEnvelope {
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.limits.expires_at.is_some_and(|expiry| now >= expiry)
    }
}

/// One debit against an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumptionEntry {
    /// The act's idempotency key. Folding deduplicates on it, so a retry cannot
    /// debit twice for one act.
    pub act_ref: String,
    pub at: DateTime<Utc>,
    pub recipients: Vec<String>,
    pub matched_predicates: Vec<String>,
    pub value_micros: Option<u64>,
}

/// An envelope and everything debited against it — the fold of its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeState {
    pub envelope: ApprovalEnvelope,
    pub consumed: Vec<ConsumptionEntry>,
}

impl EnvelopeState {
    pub fn acts_used(&self) -> u32 {
        self.consumed.len() as u32
    }

    /// How many acts already reached `recipient`.
    pub fn acts_for_recipient(&self, recipient: &str) -> u32 {
        self.consumed
            .iter()
            .filter(|entry| entry.recipients.iter().any(|had| had == recipient))
            .count() as u32
    }

    pub fn value_used_micros(&self) -> u64 {
        self.consumed
            .iter()
            .filter_map(|entry| entry.value_micros)
            .sum()
    }

    /// Whether this act was already debited — the idempotency check.
    pub fn already_debited(&self, act_ref: &str) -> bool {
        self.consumed.iter().any(|entry| entry.act_ref == act_ref)
    }
}

/// Everything a decision may be made from.
///
/// The type is the safety property. §5's rule — *"if a predicate cannot be
/// evaluated without trusting the agent, it cannot be in an envelope"* — is
/// enforced by there being nowhere in this struct to put the agent's own account
/// of what it is doing. Each field is supplied by the caller from a source the
/// agent does not write:
///
/// - `effective` comes from `execution::resolve_effective_action`, after
///   argument expansion — §4A, and the reason the typed arguments are never
///   consulted here;
/// - `consequence_class` comes from the classifier, from the act's `(capability,
///   action)` and nothing else;
/// - `engagement_identities` comes from the engagement store;
/// - `now` comes from the clock.
#[derive(Debug, Clone)]
pub struct ActFacts<'a> {
    pub act_ref: &'a str,
    pub effective: &'a EffectiveAction,
    pub consequence_class: ConsequenceClass,
    pub engagement_id: Option<&'a str>,
    pub engagement_identities: &'a [String],
    /// How many attachments are outside the asset ledger, or `None` when the
    /// caller cannot tell.
    ///
    /// `None` is **unknown, not zero**. A caller that has no attachment ledger
    /// to consult would otherwise assert "nothing is outside it" and satisfy
    /// `NoAttachmentOutsideLedger` vacuously — the same shape as an act naming
    /// no recipients passing a predicate about who it reaches. The predicate
    /// fails closed on `None`.
    pub attachments_outside_ledger: Option<usize>,
    pub value_micros: Option<u64>,
    pub now: DateTime<Utc>,
}

/// Why an act is not covered. Every variant means the same thing operationally
/// — **ask** — but they are distinct because an owner surface that cannot say
/// *why* an envelope stopped applying is a surface nobody trusts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NotCoveredReason {
    /// The default, and the whole posture: no envelope means ask.
    NoEnvelope,
    /// The mode is off or shadow. Shadow still asks (plan phase 2).
    NotEnforcing,
    /// §4A. The act accepts an escape hatch, so what an envelope would authorise
    /// is a description of the act rather than the act.
    ActionNotBindable {
        escape_hatches: Vec<String>,
    },
    /// The class is one this kind of envelope may never carry.
    ClassNotCoverable {
        class: ConsequenceClass,
        kind: String,
    },
    /// The class is coverable in principle but not listed in `covers[]`.
    ClassNotGranted {
        class: ConsequenceClass,
    },
    Revoked,
    Expired,
    /// A limit is spent. Exhaustion degrades to asking, never to proceeding.
    LimitReached {
        limit: String,
    },
    /// A boundary predicate evaluated false.
    PredicateFailed {
        predicate: String,
        detail: String,
    },
    /// A batch target the owner never reviewed.
    NotInReviewedBatch {
        recipient: String,
    },
    /// The act carries no stable identifier.
    ///
    /// The consumption ledger is idempotent **on `act_ref`**, so a blank one
    /// makes every unidentified act share a single debit slot: the first is
    /// recorded and every later one is silently treated as a replay of it. An
    /// envelope for ten acts would then authorise an unbounded number. Refusing
    /// is the only answer that keeps the cap meaning what it says.
    UnidentifiedAct,
}

/// The resolver's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum EnvelopeDecision {
    /// Proceed and debit. Names the envelope and every predicate that matched,
    /// which is §7's audit property.
    Covered {
        envelope_id: String,
        outcome: String,
        matched_predicates: Vec<String>,
    },
    /// Ask, exactly as today.
    NotCovered { reason: NotCoveredReason },
}

impl EnvelopeDecision {
    pub fn is_covered(&self) -> bool {
        matches!(self, Self::Covered { .. })
    }

    pub fn envelope_id(&self) -> Option<&str> {
        match self {
            Self::Covered { envelope_id, .. } => Some(envelope_id),
            Self::NotCovered { .. } => None,
        }
    }
}

/// How much authority the resolver actually has.
///
/// Plan phase 2 ships `Shadow`: log what *would* have been covered while still
/// asking for everything, so the boundary predicates are proved against reality
/// before they decide anything. Phase 3 turns on `Enforcing` for one program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvelopeMode {
    /// Nothing is resolved and nothing is logged. Behaviour is byte-for-byte
    /// today's, which is the first acceptance criterion.
    #[default]
    Off,
    /// Resolve and record, but always ask.
    Shadow,
    /// A covered act proceeds and debits.
    Enforcing,
}

impl EnvelopeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Enforcing => "enforcing",
        }
    }

    /// Whether a `Covered` verdict may actually let the act through.
    pub fn may_authorise(self) -> bool {
        matches!(self, Self::Enforcing)
    }
}
