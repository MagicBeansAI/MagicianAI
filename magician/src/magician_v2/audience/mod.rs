//! Who may see something — a named, enumerable set of identities with a lifetime.
//!
//! Doc: `docs/components/magician/audience.md`.
//!
//! # Why this exists
//!
//! The data room bound its access to an **engagement** — bilateral, external,
//! counterparty-shaped. That one coupling is what made the whole deal-close set
//! read as a fundraising feature, when the machinery underneath is not: *"a
//! bounded set of documents shared with a bounded set of people, whose access
//! derives from a relationship and ends with it"* describes cohort materials, a
//! client's deliverables, an audit pack and somebody's own records equally well.
//!
//! Widening the binding from *engagement* to **audience** is the whole
//! generalisation. Everything else was already generic.
//!
//! # The invariant, and why the type shape is the enforcement
//!
//! **An audience is always enumerable.** If you cannot list who is in it, it is
//! not an audience — it is *publication*, which is a different consequence class
//! entirely (`submission_or_publication`, never coverable by a standing
//! envelope; see `docs/components/magician/consequence-classes.md`).
//!
//! So there is no `Public` variant, no wildcard, and no "anyone with the link".
//! Not as a policy that could be relaxed — the type cannot express it. That
//! matters more after this generalisation than before it: "audience" *sounds*
//! like it could be open-ended in a way "engagement" never did, and the refusal
//! has to survive the rename.
//!
//! # Not a work context
//!
//! [`crate::magician_v2::work_context`] shares two variant names and answers a
//! different question: *what capability may be used here*, versus *who may see
//! this*. A program is both, which is exactly why keeping them separate matters
//! — unifying would couple capability resolution to access control, and a change
//! to one would silently move the other.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// What kind of relationship an audience is drawn from.
///
/// Every variant names something with **members you could list**. Adding one
/// that does not would break the invariant this module exists to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudienceKind {
    /// A bilateral relationship with a counterparty. The original case.
    Engagement,
    /// Everyone working a programme — a cohort, an intake, a launch.
    Program,
    /// A client or supplier, across many engagements.
    Account,
    /// A convened group: auditors, a board, a review committee.
    Panel,
    /// One named individual — their own records, their own offer.
    Person,
}

impl AudienceKind {
    /// Every arm, in declaration order.
    ///
    /// [`Self::parse`] reads this list, so a variant missing from it cannot be
    /// named by a caller that only has a string — a route, a config file, a
    /// stored record. That is the fail-closed direction (an unnameable kind
    /// refuses rather than admitting under some other kind's key), and the test
    /// that walks it is exhaustive on the enum so a new variant cannot be added
    /// without somebody deciding whether it belongs here.
    pub const ALL: [Self; 5] = [
        Self::Engagement,
        Self::Program,
        Self::Account,
        Self::Panel,
        Self::Person,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Engagement => "engagement",
            Self::Program => "program",
            Self::Account => "account",
            Self::Panel => "panel",
            Self::Person => "person",
        }
    }

    /// The kind a caller named, or `None`.
    ///
    /// **`None` is "that is not a kind", never a default.** A caller that wants
    /// a fallback picks one itself and is visible doing so; parsing that
    /// silently returned [`Self::Engagement`] for an unrecognised word would
    /// file a programme's roster, an account's or a panel's under the
    /// engagement key — and [`AudienceRef::as_key`] exists precisely so those
    /// do not merge.
    ///
    /// Case and surrounding whitespace are folded, because a kind typed on two
    /// different days is one kind. Nothing else is: there is no plural form, no
    /// synonym table and no prefix match, for the same reason the counterparty
    /// register refuses a near miss.
    pub fn parse(label: &str) -> Option<Self> {
        let wanted = label.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == wanted.as_str())
    }
}

/// A pointer to an audience.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AudienceRef {
    pub kind: AudienceKind,
    pub id: String,
}

impl AudienceRef {
    pub fn new(kind: AudienceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub fn engagement(id: impl Into<String>) -> Self {
        Self::new(AudienceKind::Engagement, id)
    }

    pub fn program(id: impl Into<String>) -> Self {
        Self::new(AudienceKind::Program, id)
    }

    pub fn account(id: impl Into<String>) -> Self {
        Self::new(AudienceKind::Account, id)
    }

    pub fn panel(id: impl Into<String>) -> Self {
        Self::new(AudienceKind::Panel, id)
    }

    pub fn person(id: impl Into<String>) -> Self {
        Self::new(AudienceKind::Person, id)
    }

    /// A stable key for paths and indexes.
    ///
    /// The kind is part of it, so `engagement:acme` and `account:acme` are
    /// different audiences. Without that, widening the binding would silently
    /// merge two relationships that happen to share an id.
    pub fn as_key(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.id)
    }

    pub fn is_named(&self) -> bool {
        !self.id.trim().is_empty()
    }
}

/// The audience itself: who is in it, and until when.
///
/// Supplied by whoever owns the relationship — an engagement store, a programme
/// roster, an account record. This module deliberately does not read any of
/// them: taking a dependency on one would tie every consumer to that one source,
/// and the whole point is that a room does not care which kind of relationship
/// it is serving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audience {
    pub reference: AudienceRef,
    /// Everyone in it. Enumerable, always — see the module note.
    pub identities: Vec<String>,
    /// When the relationship ends. `None` means it has no scheduled end, not
    /// that it is permanent — a caller may still close what it governs.
    pub expires_at: Option<DateTime<Utc>>,
}

impl Audience {
    pub fn new(reference: AudienceRef, identities: Vec<String>) -> Self {
        Self {
            reference,
            identities,
            expires_at: None,
        }
    }

    pub fn expiring_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Whether the relationship is still current.
    ///
    /// Inclusive of the instant it ends, like every other expiry in this
    /// codebase: expiring at noon means expired at noon.
    pub fn is_current(&self, now: DateTime<Utc>) -> bool {
        !self.expires_at.is_some_and(|expiry| now >= expiry)
    }

    /// Whether `identity` is in this audience **now**.
    ///
    /// Membership and currency are checked together on purpose. Somebody who was
    /// on a relationship that has since ended is not a member — treating "was
    /// listed" as "may see" is how access outlives the relationship it came
    /// from, which is the failure the whole model is built to prevent.
    pub fn admits(&self, identity: &str, now: DateTime<Utc>) -> bool {
        self.is_current(now) && self.identities.iter().any(|held| held == identity)
    }

    /// How many people are in it. An audience with none admits nobody, which is
    /// a legitimate state — a room assembled before anyone is invited.
    pub fn size(&self) -> usize {
        self.identities.len()
    }
}
