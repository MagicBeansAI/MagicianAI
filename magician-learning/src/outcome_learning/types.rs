//! What an outcome observation is — plan §3.
//!
//! Generic on purpose. Nothing here mentions fundraising, accelerators or email:
//! an observation is *"this variant of this act produced this result"*, which is
//! the same shape whether the act was a pitch, a support reply or a proposal.
//!
//! # The constraints that shape the types
//!
//! §5 lists five things this must not do, and three of them are enforced by what
//! these types **cannot express**:
//!
//! - **Not observe the owner.** There is no field for the operator. The subject
//!   of an observation is always a counterparty and an act, so recording the
//!   person running the system is not something a caller can accidentally do.
//! - **Not confirm itself.** [`variant_version`](OutcomeObservation::variant_version)
//!   is required, not optional. It is the cohort key, and the before/after
//!   comparison across versions *is* the evidence — self-confirmation is
//!   prevented by cohort separation rather than by discarding post-change data.
//! - **Not call silence a result too early.** [`OutcomeLabel::Silent`] is the one
//!   label that means "nothing happened", so it is only meaningful once its
//!   window has closed. The store refuses it before then.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeLabel {
    Replied,
    /// Nothing came back. **Only meaningful once the window has closed** — see
    /// [`OutcomeLabel::requires_maturity`].
    Silent,
    Accepted,
    Rejected,
    /// A room was visited. Observable because visiting is an act on our own
    /// surface; §3 is explicit that no beacon is ever added to an email to
    /// manufacture this.
    Opened,
    Progressed,
}

impl OutcomeLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replied => "replied",
            Self::Silent => "silent",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Opened => "opened",
            Self::Progressed => "progressed",
        }
    }

    /// Whether this label is only meaningful after a waiting period.
    ///
    /// Silence is the absence of a signal, so it is indistinguishable from
    /// "not yet" until the window closes. Every other label is an event that
    /// happened, and an event is true the moment it occurs.
    pub fn requires_maturity(self) -> bool {
        matches!(self, Self::Silent)
    }

    /// Whether this label says the counterparty did something.
    ///
    /// Useful to a later analysis phase, and defined here so "engagement" means
    /// one thing rather than being re-derived per caller.
    pub fn is_engagement(self) -> bool {
        matches!(
            self,
            Self::Replied | Self::Accepted | Self::Opened | Self::Progressed
        )
    }
}

/// What the carrier reported, which is not the same as what the recipient did.
///
/// §3 allows exactly these because the restricted outward adapters already
/// report them. A bounced send that shows as `silent` would otherwise be counted
/// as a counterparty ignoring us, when in fact nothing arrived — the single most
/// misleading confusion available to a loop like this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Accepted,
    Delivered,
    Bounced,
    Complained,
    Unknown,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Delivered => "delivered",
            Self::Bounced => "bounced",
            Self::Complained => "complained",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the act plausibly reached a person.
    ///
    /// A bounce or an unknown state means an outcome says nothing about the
    /// counterparty — it says something about the carrier. An analysis that
    /// mixes the two learns about mail servers.
    pub fn reached_someone(self) -> bool {
        matches!(self, Self::Accepted | Self::Delivered)
    }
}

/// Something true about the situation that is not the thing being tested.
///
/// §3: *"Thirty accelerators is a small sample and the strongest effect will
/// usually be warm versus cold, not anything about the copy. A loop that cannot
/// see that will confidently attribute an introducer's effect to a subject
/// line."*
///
/// Free-form `kind`/`value` rather than an enum, deliberately: the confounders
/// that matter are domain-specific and discovered late, and a closed set would
/// force the interesting ones to be recorded as nothing at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confounder {
    pub kind: String,
    pub value: String,
}

impl Confounder {
    pub fn new(kind: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            value: value.into(),
        }
    }
}

/// One recorded outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeObservation {
    pub observation_id: String,
    pub engagement_id: Option<String>,
    pub program_id: Option<String>,
    /// The act this is an outcome of — the send, the answer, the share, the
    /// meeting. Links an observation back to the outward assertions record, so
    /// "what did we actually say" is answerable from an outcome.
    pub act_ref: String,
    /// Which variant was used: which answer, which framing, which template.
    pub variant_ref: String,
    /// **The cohort key.** Which proposal version was active when this act
    /// happened. Required, because comparing outcomes across versions is the
    /// only thing that distinguishes learning from self-confirmation.
    pub variant_version: String,
    pub label: OutcomeLabel,
    pub delivery_state: DeliveryState,
    pub observed_at: DateTime<Utc>,
    /// When silence became meaningful. Required for [`OutcomeLabel::Silent`] and
    /// meaningless for everything else.
    pub matured_at: Option<DateTime<Utc>>,
    pub confounders: Vec<Confounder>,
}

impl OutcomeObservation {
    /// Whether this observation may be used as evidence about a counterparty.
    ///
    /// Two ways it may not: the act never reached anyone, or it is a silence
    /// whose window has not closed. Both would otherwise read as "they ignored
    /// us", which is the confusion §3's delivery signals exist to prevent.
    pub fn is_usable_evidence(&self, now: DateTime<Utc>) -> bool {
        if !self.delivery_state.reached_someone() {
            return false;
        }
        if self.label.requires_maturity() {
            return self.matured_at.is_some_and(|matured| now >= matured);
        }
        true
    }
}

/// What a caller supplies. The store owns `observation_id`, so a caller cannot
/// mint one that collides with another observation.
#[derive(Debug, Clone)]
pub struct RecordOutcome {
    pub engagement_id: Option<String>,
    pub program_id: Option<String>,
    pub act_ref: String,
    pub variant_ref: String,
    pub variant_version: String,
    pub label: OutcomeLabel,
    pub delivery_state: DeliveryState,
    pub matured_at: Option<DateTime<Utc>>,
    pub confounders: Vec<Confounder>,
}
