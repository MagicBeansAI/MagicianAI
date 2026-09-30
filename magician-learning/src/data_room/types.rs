//! What a data room is — plan §3.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::audience::{Audience, AudienceRef};

/// Who, among the audience, may see one document.
///
/// §3: *"per-identity, so one room can differ per reader."* Phase 5 is where
/// that becomes selective; phase 1 records the intent so a document added now
/// does not have to be re-decided later.
///
/// **`Everyone` means every identity in the audience — never the public.** There
/// is no variant for "anyone with the link", and there will not be one. An
/// audience is enumerable by construction (`magician::magician_v2::audience`), so a
/// reader outside it is not a reader at all — whatever kind of relationship the
/// audience was drawn from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "visibility", rename_all = "snake_case")]
pub enum DocumentVisibility {
    /// Every identity in the audience.
    Everyone,
    /// Only these identities.
    ///
    /// **Not validated against the audience at write time**, because the roster
    /// belongs to whoever owns the relationship and this module deliberately has
    /// no dependency on any of them. It does not need one:
    /// [`permits`](Self::permits) checks audience membership on every read, so
    /// naming an identity outside it grants nothing. Listing a stranger here is
    /// therefore inert rather than dangerous.
    Identities { identities: Vec<String> },
}

impl DocumentVisibility {
    /// Whether `identity` may see this document, given the audience.
    ///
    /// Checks **audience membership first**, for both variants — and membership
    /// includes the relationship still being current. A document restricted to a
    /// named identity must still refuse someone whose relationship has ended,
    /// otherwise a per-document allow-list would quietly outlive the thing it
    /// belongs to.
    pub fn permits(&self, identity: &str, audience: &Audience, now: DateTime<Utc>) -> bool {
        if !audience.admits(identity, now) {
            return false;
        }
        match self {
            Self::Everyone => true,
            Self::Identities { identities } => identities.iter().any(|held| held == identity),
        }
    }
}

/// The separator §4's `artifact_ref@revision` form uses.
pub const REVISION_SEP: char = '@';

/// Split a document reference into `(artifact, revision)`.
///
/// Splits on the **last** separator, because an artifact reference may
/// legitimately contain one — an email-shaped id, a URI with a user part —
/// while the pin this form adds is always the trailing segment. Both halves
/// must be non-empty: a trailing `@` names no revision and a leading one names
/// no artifact, and reading either as a pin would match documents that pinned
/// nothing.
///
/// Lives here rather than in the caller that needed it first, so the store's
/// write guard and every consumer that has to *interpret* a stored reference
/// agree by construction. Two functions that must answer *"which revision is
/// this"* and do not share code will disagree eventually, and here the
/// disagreement is a correction that misses a room.
pub fn split_document_ref(document_ref: &str) -> (&str, Option<&str>) {
    match document_ref.rsplit_once(REVISION_SEP) {
        Some((artifact, revision)) if !artifact.is_empty() && !revision.is_empty() => {
            (artifact, Some(revision))
        },
        _ => (document_ref, None),
    }
}

/// Whether a document reference **unambiguously** names an immutable revision.
///
/// Stricter than [`split_document_ref`] on purpose, and the asymmetry is the
/// point: the WRITE decides what may enter a room and can afford to refuse
/// anything it cannot read exactly, while the READ has to make sense of
/// whatever is already on file.
///
/// # One separator, or it is refused
///
/// Splitting on the last `@` is the only sensible rule for reading, and it
/// cannot tell an unpinned reference that happens to contain one from a pinned
/// one. `mailto:a@b.test` reads as artifact `mailto:a` at revision `b.test` —
/// a revision that matches nothing, so a retraction asking *"which rooms carry
/// this"* would silently **not** flag that room. A false negative on a
/// correction is the direction that leaves a wrong figure in front of somebody,
/// which is the whole failure this pin exists to prevent.
///
/// So a reference carrying more than one separator is refused rather than
/// guessed at. `artifact://deck@r2` has exactly one. A reference whose artifact
/// half genuinely contains an `@` has to be spelled some other way, and being
/// told that at the write is far better than discovering it during a
/// correction.
pub fn names_a_revision(document_ref: &str) -> bool {
    document_ref.matches(REVISION_SEP).count() == 1 && split_document_ref(document_ref).1.is_some()
}

/// One document in a room.
///
/// A **reference**, never a copy (§4): a copy would diverge from the version the
/// owner edits, and the point of a room is that what a counterparty sees is what
/// we have.
///
/// And a reference to an **immutable revision**, not to "latest" — see
/// [`split_document_ref`]. A room serving a moving pointer silently shows a
/// different document than the one that was cleared, which is worse than
/// showing a stale one: stale is visible to whoever compares, and swapped is
/// not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentEntry {
    pub artifact_ref: String,
    pub visibility: DocumentVisibility,
    pub added_at: DateTime<Utc>,
    pub added_by: String,
    /// Set when withdrawn. The entry is **kept**, because "this was in the room
    /// between March and April" is the question an audit asks, and a deleted row
    /// cannot answer it.
    pub withdrawn_at: Option<DateTime<Utc>>,
}

impl DocumentEntry {
    pub fn is_present(&self) -> bool {
        self.withdrawn_at.is_none()
    }
}

/// Why a room is not open, or that it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomStanding {
    Open,
    /// A clock closed it — the room's own, or the relationship's.
    Expired,
    /// Closed deliberately.
    Closed,
}

impl RoomStanding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Expired => "expired",
            Self::Closed => "closed",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// The document channel of one audience.
///
/// §3 calls it *"the document channel of an engagement"*; an engagement is one
/// kind of audience, and everything here works the same for the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRoom {
    pub room_id: String,
    /// Who it is for. **Access derives from this** — there is no second access
    /// model, and adding one is what would let a room outlive the relationship.
    ///
    /// An audience rather than an engagement: the same container serves a deal
    /// room, a cohort's materials, a client's deliverables and an audit pack,
    /// and nothing about the room changes between them.
    pub audience: AudienceRef,
    pub documents: Vec<DocumentEntry>,
    pub opened_at: DateTime<Utc>,
    pub opened_by: String,
    /// The room's own clock, separate from the audience's.
    ///
    /// §3 has the room closing on the relationship's clock, and it still does —
    /// [`visible_to`](Self::visible_to) requires the audience to be current. This
    /// is the room closing *earlier* than the relationship, which is the common
    /// case: a data pack for a round that ends before the account does.
    pub closes_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
}

impl DataRoom {
    /// Whether the room is open at `now`.
    ///
    /// Derived, never stored. A stored status would be a second source of truth
    /// that drifts the moment a room expires with nobody writing to it — the
    /// same reason envelope standing is derived.
    pub fn standing(&self, now: DateTime<Utc>) -> RoomStanding {
        if self.closed_at.is_some() {
            RoomStanding::Closed
        } else if self.closes_at.is_some_and(|closes| now >= closes) {
            RoomStanding::Expired
        } else {
            RoomStanding::Open
        }
    }

    /// Documents currently in the room.
    pub fn present_documents(&self) -> Vec<&DocumentEntry> {
        self.documents
            .iter()
            .filter(|entry| entry.is_present())
            .collect()
    }

    /// What one identity would see, if the room were readable.
    ///
    /// Phase 1 has no reader-facing path — this exists so the owner can check
    /// *"what would this person see"* before anything is ever shared, which is
    /// the whole value of assembling a room early.
    ///
    /// Returns nothing when the room is not open, whatever the per-document
    /// visibility says: a closed room shows nobody anything, and evaluating
    /// document rules first would let a per-document allow-list read as access.
    pub fn visible_to<'a>(
        &'a self,
        identity: &str,
        audience: &Audience,
        now: DateTime<Utc>,
    ) -> Vec<&'a DocumentEntry> {
        // Two clocks, both of which must be running: the room's own, and the
        // relationship's. A room left open past the end of the audience it
        // serves would be exactly the "access outlives the relationship" failure
        // the model exists to prevent.
        if !self.standing(now).is_open() || !audience.is_current(now) {
            return Vec::new();
        }
        // The audience must be the one this room was opened for. A roster handed
        // in from somewhere else would grant access the room never agreed to.
        if audience.reference != self.audience {
            return Vec::new();
        }
        self.documents
            .iter()
            .filter(|entry| entry.is_present())
            .filter(|entry| entry.visibility.permits(identity, audience, now))
            .collect()
    }
}

/// What a caller supplies to open a room. `room_id` is the store's to decide.
#[derive(Debug, Clone)]
pub struct OpenDataRoom {
    pub audience: AudienceRef,
    pub opened_by: String,
    pub closes_at: Option<DateTime<Utc>>,
}
