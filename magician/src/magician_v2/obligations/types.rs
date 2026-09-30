//! What is owed, and by when — composable work modules, Module D.
//!
//! Contract from §6: an obligation is
//! `(relationship, what, due_at, owed_by_us | owed_to_us)`; it surfaces on the
//! owning agent's cycle and escalates when it lapses.
//!
//! The plan writes the first term as *engagement*; it is an
//! [`AudienceRef`](crate::magician_v2::audience::AudienceRef), of which an
//! engagement is one kind. Promises are made to clients, cohorts, panels and
//! individuals as readily as to counterparties, and *"send me your metrics by
//! Friday"* is the same obligation whoever said it.
//!
//! # Why this is not "stage"
//!
//! Program running state tracks **stage** — where something has got to. It does
//! not track **obligation** — what somebody said they would do. If a
//! counterparty says *"send me your metrics by Friday"*, nothing about the stage
//! enforces Friday, and the review found nothing that owned that.
//!
//! # Generic
//!
//! *"Reusable everywhere. Nothing about it is fundraising-specific."* An
//! obligation is a promise with a deadline and a direction — the same shape for
//! a deck owed to an investor, a reply owed to a customer, or a document a
//! supplier owes us.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::audience::AudienceRef;

/// Who owes whom.
///
/// The distinction is the whole point of recording direction: a lapse means two
/// different things, and conflating them would surface *"you are late"* for
/// something the other side owes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationDirection {
    /// We promised it. A lapse is our failure, and it is the owner's problem to
    /// act on.
    OwedByUs,
    /// They promised it. A lapse is a prompt to follow up, not a failure.
    OwedToUs,
}

impl ObligationDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwedByUs => "owed_by_us",
            Self::OwedToUs => "owed_to_us",
        }
    }

    /// Whether a lapse is something **we** did.
    ///
    /// Drives how a lapse is surfaced, so the two never read alike: a broken
    /// promise of ours and an unanswered request of theirs need different words
    /// and different urgency.
    pub fn lapse_is_ours(self) -> bool {
        matches!(self, Self::OwedByUs)
    }
}

/// How an obligation ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "settlement", rename_all = "snake_case")]
pub enum Settlement {
    /// It was done.
    Met { note: Option<String> },
    /// It stopped applying — the engagement ended, the ask was withdrawn, the
    /// thing became irrelevant.
    ///
    /// Distinct from `Met` because *"we did it"* and *"we no longer have to"*
    /// are different facts, and an obligation register that conflated them would
    /// report a hit rate that was partly wishful.
    Released { reason: String },
}

impl Settlement {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Met { .. } => "met",
            Self::Released { .. } => "released",
        }
    }
}

/// Where an obligation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationState {
    /// Live and not yet due.
    Open,
    /// Live and past its deadline.
    Lapsed,
    Met,
    Released,
}

impl ObligationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Lapsed => "lapsed",
            Self::Met => "met",
            Self::Released => "released",
        }
    }

    /// Whether anything is still owed.
    pub fn is_outstanding(self) -> bool {
        matches!(self, Self::Open | Self::Lapsed)
    }
}

/// One promise with a deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Obligation {
    pub obligation_id: String,
    /// The relationship the promise was made in.
    pub audience: AudienceRef,
    pub program_id: Option<String>,
    /// What was promised, in the words it was promised in.
    ///
    /// Free text on purpose. An obligation the owner cannot recognise at a
    /// glance is one they cannot act on, and a taxonomy of promise-types would
    /// be a classification exercise that adds nothing: the deadline and the
    /// direction are what drive behaviour.
    pub what: String,
    pub due_at: DateTime<Utc>,
    pub direction: ObligationDirection,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    /// The act that created the promise, when there was one — the send, the
    /// meeting statement. Links an obligation back to what was actually said.
    pub source_act_ref: Option<String>,
    pub settled_at: Option<DateTime<Utc>>,
    pub settlement: Option<Settlement>,
}

impl Obligation {
    /// Where this stands at `now`.
    ///
    /// **Lapsing is derived from the clock, never stored.** A stored `lapsed`
    /// flag needs somebody to write it, and the one thing an obligation register
    /// must not depend on is a sweep having run — the whole point is to catch
    /// what nobody remembered.
    pub fn state(&self, now: DateTime<Utc>) -> ObligationState {
        match &self.settlement {
            Some(Settlement::Met { .. }) => ObligationState::Met,
            Some(Settlement::Released { .. }) => ObligationState::Released,
            None if now >= self.due_at => ObligationState::Lapsed,
            None => ObligationState::Open,
        }
    }

    pub fn is_outstanding(&self, now: DateTime<Utc>) -> bool {
        self.state(now).is_outstanding()
    }

    /// How overdue this is, or `None` if it is not.
    pub fn overdue_by(&self, now: DateTime<Utc>) -> Option<chrono::Duration> {
        (self.state(now) == ObligationState::Lapsed).then(|| now - self.due_at)
    }
}

/// What a caller supplies. `obligation_id` and the settlement fields are the
/// store's, so a caller cannot hand in a pre-settled promise.
#[derive(Debug, Clone)]
pub struct RecordObligation {
    pub audience: AudienceRef,
    pub program_id: Option<String>,
    pub what: String,
    pub due_at: DateTime<Utc>,
    pub direction: ObligationDirection,
    pub created_by: String,
    pub source_act_ref: Option<String>,
}
