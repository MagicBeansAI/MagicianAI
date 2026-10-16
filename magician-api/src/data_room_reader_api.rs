//! **The reader side of a shared room** — a capability URL in, a listing or a
//! document out, and an access record written before either.
//!
//! Doc: `docs/components/magician/data-room.md`. Plan:
//! `docs/plans/2026-08-07-opc-deal-close.md` (phases 2, 3 and 5, which only
//! finish here).
//!
//! Five modules were built and could not be reached: rooms
//! ([`DataRoomStore`]), grants ([`ShareLinkStore`]), the access lane
//! ([`AccessStore`]), the per-identity visibility rule
//! ([`DataRoom::visible_to`]) and the notice a room must carry
//! ([`ROOM_LOGGING_NOTICE`]). This is the surface that joins them, and it is
//! the only place in the codebase where somebody outside the workspace is
//! served anything at all.
//!
//! # Nothing here is about fundraising
//!
//! A room is *a bounded set of documents shared with a bounded set of people,
//! whose access derives from a relationship and ends with it*. That is a
//! diligence pack, a cohort's materials, a client's deliverables and an audit
//! bundle equally. The vocabulary in this file is rooms, readers, documents and
//! relationships, and no consumer's language leaks into it.
//!
//! # Every step is a refusal point
//!
//! In order, and each one is capable of ending the request:
//!
//! 1. **Shape.** A scope, room id or document reference carrying `U+001F` —
//!    the separator every derived id in this set is built from — is refused
//!    before any store is touched, because a store's own refusal would arrive
//!    *after* [`ShareLinkStore::present`] had already spent a presentation.
//!    A room id that is not a derived id is refused for the same reason plus
//!    a blunter one: [`DataRoomStore::load`] interpolates the id into a path.
//! 2. **The credential.** The secret is presented against the **living**
//!    audience, so a revoked link, a lapsed one, a link bound to another
//!    relationship, a relationship that has ended and an identity dropped from
//!    the roster all refuse — and the single decision lives in
//!    [`ShareLinkStore::present`], never re-implemented here. A second copy of
//!    that gate is exactly the thing that would one day disagree with it.
//!    The **roster's** own refusals are held behind this same gate. The roster
//!    is read before it — the credential is checked against the roster, so it
//!    has to be — but what the roster decided is only ever said to somebody
//!    whose secret named a link on this room. A roster refusal reports the
//!    state of a relationship, and a caller who has presented nothing has
//!    proved no connection to one.
//! 3. **The record.** A successful presentation is written to the access lane
//!    **before anything is served**. A room that hands over a document without
//!    recording the visit is the unaccountable disclosure this whole plan
//!    exists to prevent, so a lane that cannot be written is a `503` and an
//!    empty response body — never a served document.
//! 4. **Visibility.** The listing is [`DataRoom::visible_to`] for *that*
//!    identity. A restricted document is **absent from the listing**, not
//!    merely unfetchable: naming a document a reader may not open discloses
//!    that it exists, who it might be for, and what the next conversation is
//!    about.
//! 5. **Both clocks.** The room's own and the relationship's. A room whose
//!    engagement has lapsed says so plainly — `410 Gone` with a message that
//!    tells the reader to ask — rather than `404`ing, because a reader who
//!    thinks the link is broken retries it and a reader who is told it ended
//!    asks a person. A room whose engagement was **never recorded** gets its
//!    own refusal instead of that one, once the reader has named a credential
//!    (step 2): it never ran, so nothing about it ended, and telling a reader
//!    it ended sends them asking after a withdrawal nobody made.
//!
//! # The secret is never logged and never echoed
//!
//! Only its blake3 hash is ever stored, by the grant layer. This surface takes
//! it from an `X-Room-Key` header when there is one and from the `k` query
//! parameter otherwise — a capability *URL* has to survive being a URL — and
//! neither request type derives `Debug`, so a stray `{:?}` cannot put a working
//! credential in a log line. No response body, refusal message or error path
//! carries it back.
//!
//! # A visit is an opening of the room, and a presentation is not a visit
//!
//! The credential is presented on **every** request — that is the
//! authorisation, and it is what stops a revoked link from opening anything —
//! and **exactly one** access event is written per successful presentation,
//! refused or served. But [`Presentation::sequence`] numbers the *grant slot's*
//! presentations, and this surface does not write it into the log:
//! `AccessEvent::sequence` is derived from the access lane instead, by
//! `visit_number`.
//!
//! The rule is the one `access_store` states and tests — *one sitting that
//! views the index and then opens a document is two events and one visit*. So
//! `GET /rooms/{room_id}`, the reader opening the room, takes the token's next
//! number, and a document fetch carries the number of the opening it follows.
//! A token whose first recorded act is a document fetch opens its first visit
//! with it, because the lane refuses a sequence of zero and a zero would read
//! as a room nobody has opened.
//!
//! `TokenAttention::visits` is the highest sequence seen, so it counts the
//! times **the room was opened**, not the times a URL was fetched: a reader who
//! opens the room once and reads four documents is one visit and five
//! presentations, and is `OpenedOnce`.
//!
//! Two consumers read that signal and only one collapses it, which is why the
//! log's number cannot be the presentation count. `derive_follow_ups` treats
//! *opened once* and *opened repeatedly* identically (*"repeat visits change
//! the conversation, not the obligation shape"*) — but `data_room::cycle` does
//! not: it splits `ReadThenSilent` from `ReturnedThenSilent` on the number and
//! prints it to the room's owner (*"has opened room X 3 times"*). A
//! presentation count arriving there reports everyone who opened a document as
//! somebody who came back.
//!
//! What this deliberately does **not** do is fold two openings into one
//! sitting. Two loads of the room a minute apart are two visits exactly as two
//! a week apart are: the room cannot tell a reload from a return, and nothing
//! here guesses — no cookie, no window, no session — because a sitting is not
//! observable from a stateless request and a guessed one would be a number
//! nobody could audit.
//!
//! # What is recorded is what was disclosed
//!
//! `document_ref` names a document **only when that document is about to be
//! served**. A refused fetch records a document-less visit instead: writing the
//! reference would put *"they opened the financials"* in the audit log about a
//! reader who was turned away, and writing nothing would make an authenticated
//! visit invisible and leave a hole in the sequence.
//!
//! # No log is read or written here
//!
//! Every read and append goes through the stores, which go through
//! `magician_v2::jsonl` — where "absent" and "unreadable" keep their two
//! meanings apart. This surface does no file IO of its own, so it cannot
//! reintroduce the fail-open that module exists to close.

use std::sync::Arc;

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{Audience, AudienceKind, AudienceRef};
use magician::magician_v2::counterparties::{CounterpartyScope, CounterpartyStore};
use magician::magician_v2::engagements::EngagementStore;
use magician::magician_v2::share_links::{
    Presentation, PresentationRefused, ShareLinkScope, ShareLinkStore,
};
use magician_learning::data_room::access_store::{AccessScope, AccessStore};
use magician_learning::data_room::{
    AccessEvent, DataRoom, DataRoomScope, DataRoomStore, RoomStanding, UserAgentClass,
    ROOM_LOGGING_NOTICE,
};

use crate::scope::resolve_required_scope;

/// Where a reader may put the secret instead of the query string. Preferred:
/// a header does not land in access logs, `Referer` headers or browser
/// history the way a query parameter does.
const READER_KEY_HEADER: &str = "X-Room-Key";

/// The separator every derived id in this set is built from.
const FIELD_SEP: char = '\u{1f}';

/// The shape [`DataRoomStore`] derives: `room-` and 32 lowercase hex digits.
const ROOM_ID_PREFIX: &str = "room-";
const ROOM_ID_DIGITS: usize = 32;

// ── Who is in the relationship ──────────────────────────────────────────────

/// **What a roster source can honestly answer.**
///
/// Three answers where there used to be two, because two of the facts this
/// port has to carry are opposites and were rendering identically. A reference
/// nothing has ever been engaged under used to come back as a `Roster` stamped
/// closed at `now` — indistinguishable, downstream, from a relationship that
/// ran and was revoked. Both refused, so nothing was ever over-served; but
/// every message either party then saw said the relationship had **ended**, and
/// steered them at revoke, withdraw and close. None of those three can bring
/// into being a relationship nobody ever recorded, so the one act that would
/// have helped was the one nothing named.
///
/// `Err` from the port is a fourth thing again — the source could not be read.
/// It stays an `Err` rather than becoming a variant here, because a fault is
/// not a decision anybody made, and folding it in is how "the disk was down"
/// starts being reported as "you are not allowed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudienceStanding {
    /// The relationship is on file, and this is its roster stamped with
    /// whatever end it runs to.
    ///
    /// Not a verdict: this variant carries the roster and nothing else, and a
    /// caller still has to ask [`Audience::is_current`] whether the end has
    /// already passed.
    Roster(Audience),
    /// The counterparty is on file and **nothing has ever put us in a
    /// relationship with them** — no engagement at all, live, lapsed or
    /// revoked.
    ///
    /// Only [`AudienceKind::Engagement`] can produce this, because the
    /// engagement roster is the only store in this tree that records a
    /// relationship's existence. A reference of the other four kinds is either
    /// held by the counterparty register or [`AudienceStanding::Unknown`].
    NeverEngaged,
    /// No such counterparty, so there is no roster to speak of. Unknown is
    /// never permission.
    Unknown,
}

/// **The living roster of a relationship**, supplied by whoever owns it.
///
/// The room deliberately has no access model of its own and the grant layer
/// deliberately reads no store, so somebody has to answer *"who is on this
/// relationship right now"* — and it must be answered live, at read time, or
/// access outlives the relationship it came from.
///
/// Every [`AudienceStanding`] other than [`AudienceStanding::Roster`] is a
/// refusal, and so is an `Err` — an unknown, a never-established and an
/// unreadable roster are none of them permission. They are kept apart only so
/// the party who is refused can be told which of *ask a person*, *record the
/// relationship* and *try again* is the one that would work.
///
/// # Why the port is async, and why it is handed `now`
///
/// A roster is two facts, not one: **who** is on the relationship, and **until
/// when** it runs. The register that answers the first is a synchronous file
/// read; the store that knows the second — `EngagementStore` — is async. A
/// synchronous port could therefore only ever answer with the membership and
/// never with its lifetime, which is exactly the state this surface was in:
/// [`Audience::expires_at`] was `None` on every audience ever produced here, so
/// [`Audience::is_current`] answered `true` for every relationship that had ever
/// existed and the second clock the room leans on
/// ([`DataRoom::visible_to`], [`crate::data_room_api::OwnerSurface`]) passed
/// vacuously. A check that always says yes is worse than no check, because it
/// reads as enforcement.
///
/// `now` is the caller's clock rather than one read here, for the same reason
/// every other expiry in this file takes it: one request decides at one instant,
/// and a source that sampled its own clock could answer "live" a microsecond
/// after the room's own standing said "closed".
#[async_trait::async_trait]
pub trait AudienceSource: Send + Sync {
    async fn living_audience(
        &self,
        principal: &str,
        workspace: &str,
        reference: &AudienceRef,
        now: DateTime<Utc>,
    ) -> Result<AudienceStanding>;
}

/// The counterparty register as a roster source.
///
/// [`CounterpartyStore::audience_for`] exposes **only verified** identities, so
/// an organisation nobody has proved an address for yields an audience that
/// admits nobody — and an empty audience admits nobody rather than everybody,
/// which is the property this whole surface leans on.
///
/// A relationship the register does not hold answers `None` rather than
/// erroring, matching `counterparty_for_engagement`: not-held is a fact, not a
/// fault. A merged-away organisation resolves to the record that survived, and
/// because the resolved reference then differs from the one the room was opened
/// for, [`ReaderSurface`] refuses rather than serving — a room pointed at a
/// folded-away id has to be re-pointed by its owner, not silently re-aimed.
///
/// # The relationship's end comes from the engagement, never from the register
///
/// A counterparty record has no scheduled end of its own — `audience_for` says
/// so and sets no expiry. The **engagement** is what runs out, so for an
/// [`AudienceKind::Engagement`] reference this asks [`EngagementStore`] whether
/// any engagement with this counterparty is still live and, if so, until when.
/// Without that step the audience carries no expiry and the room's second clock
/// is decorative.
pub struct CounterpartyAudiences {
    store: CounterpartyStore,
    /// The roster that knows when an engagement ends. **Required, and never
    /// resolved at read time.**
    ///
    /// This field used to be an `Option` that fell back to the process-wide
    /// engagement store per request and, when that store had never been
    /// installed, logged a warning and returned the audience *unstamped*. An
    /// unstamped audience is [`Audience::is_current`] answering `true` for
    /// ever, which is both second-clock guards — [`DataRoom::visible_to`] and
    /// [`crate::data_room_api::OwnerSurface`]'s `relationship_ended` — passing
    /// vacuously. So a binary that mounted these routes without installing the
    /// store served every engagement room on a relationship that could never
    /// end, a revoked engagement kept opening the room, and the only evidence
    /// was a log line on a path nobody watches.
    ///
    /// Holding it here instead makes that unbuildable rather than unlikely: a
    /// binary that has no roster cannot construct the source, so it fails at
    /// startup where the person who can fix it is standing, and there is no
    /// longer any request-time branch that can answer "unstamped".
    engagements: Arc<EngagementStore>,
}

impl CounterpartyAudiences {
    /// The roster is an argument because it is a dependency, not a setting.
    ///
    /// There is deliberately no constructor that reaches for the process-wide
    /// store: a fallible one would push the same failure back to whichever
    /// caller ignored its `Result`, and an infallible one could only paper over
    /// the absence again.
    pub fn new(store: CounterpartyStore, engagements: Arc<EngagementStore>) -> Self {
        Self { store, engagements }
    }
}

#[async_trait::async_trait]
impl AudienceSource for CounterpartyAudiences {
    async fn living_audience(
        &self,
        principal: &str,
        workspace: &str,
        reference: &AudienceRef,
        now: DateTime<Utc>,
    ) -> Result<AudienceStanding> {
        let scope = CounterpartyScope::new(principal, workspace);
        if self.store.load(&scope, &reference.id)?.is_none() {
            return Ok(AudienceStanding::Unknown);
        }
        let audience = self
            .store
            .audience_for(&scope, &reference.id, reference.kind)?;

        // Only an engagement has a store in this tree that records when it
        // ends. A programme, an account, a panel and a person do not: nothing
        // here knows the last day of a cohort, of an account relationship, of a
        // convened panel or of somebody's own records, so their audiences stay
        // unstamped and their second clock is STILL vacuous. That is a named
        // remaining gap, not something this closes — the room's own closing
        // date and the link's expiry are the only bounds those four have.
        if reference.kind != AudienceKind::Engagement {
            return Ok(AudienceStanding::Roster(audience));
        }

        // No fallback and no "is a roster installed" branch: the roster is a
        // field, so by the time this runs there is one. The absent case is a
        // construction error, not a request-time one — see the field's note.
        let roster = &self.engagements;

        // The head the register resolved to, which is what `audience_for`
        // stamped on the reference — merge edges already followed. Comparing
        // against `reference.id` instead would miss an engagement recorded
        // under a name that has since been folded into another organisation.
        let head = audience.reference.id.clone();
        let records = roster.list(principal, workspace).await;

        // ONE register read for every label, not two per engagement.
        //
        // `resolve_label` costs a `load` and a `summary` — two full reads and
        // parses of the register each — and this runs on an UNAUTHENTICATED
        // reader request, so a loop of them would put `2N` full parses on every
        // page a counterparty opens. The batch form answers identically: merge
        // edges followed, a label the register does not hold resolving to
        // nothing rather than to a near miss.
        let labels: Vec<String> = records
            .iter()
            .map(|record| record.counterparty.clone())
            .collect();
        let resolved = self.store.resolve_labels(&scope, &labels)?;

        // Liveness is `live_authority`'s answer and never a re-reading of
        // `revoked_at_ms` / `expires_at_ms` here: one clock, one decider. The
        // expiry instant is then read off the record, because `LiveAuthority`
        // carries what may be done and not until when.
        let mut latest: Option<i64> = None;
        // Whether ANY engagement with this counterparty is on file at all —
        // live, lapsed or revoked. `list` returns revoked and expired records
        // too, which is what makes this answerable: an empty match set is
        // "never engaged" and can be nothing else.
        let mut ever_engaged = false;
        for record in &records {
            let matches = resolved
                .get(&record.counterparty)
                .and_then(Option::as_ref)
                .is_some_and(|engaged| engaged.as_str() == head.as_str());
            if !matches {
                continue;
            }
            ever_engaged = true;
            if roster
                .live_authority(
                    principal,
                    workspace,
                    &record.engagement_id,
                    now.timestamp_millis(),
                )
                .await
                .is_err()
            {
                continue;
            }
            latest = Some(match latest {
                Some(held) => held.max(record.expires_at_ms),
                None => record.expires_at_ms,
            });
        }

        // Never engaged is its own answer, and not because it is any less of a
        // refusal — it is reported separately because it is a different fact
        // with a different remedy. Stamping it closed at `now`, as this used
        // to, made it arrive downstream as a relationship that ENDED, and the
        // acts an ending suggests (revoke, withdraw, close) are precisely the
        // three that cannot create a relationship nobody recorded.
        if !ever_engaged {
            return Ok(AudienceStanding::NeverEngaged);
        }

        // Every remaining fold closes the room, and all of them are the
        // fail-closed direction. Engagements exist but none is live — revoked
        // or expired — means the relationship is not running, and a
        // relationship that is not running is not one a counterparty may keep
        // reading under. An `expires_at_ms` outside the range a `DateTime` can
        // hold is a date nobody can compare against, and a bound that cannot be
        // read is not a bound; `now` closes the room in both cases, rather than
        // the record's own future expiry, because a revoked engagement's
        // scheduled end is still ahead of us and honouring it would keep the
        // room open on a relationship the owner already killed.
        let expiry = latest.and_then(DateTime::<Utc>::from_timestamp_millis);
        Ok(AudienceStanding::Roster(
            audience.expiring_at(expiry.unwrap_or(now)),
        ))
    }
}

// ── What a reader asks for ──────────────────────────────────────────────────

/// The tenant the room belongs to. Naming it is not permission — the secret is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderScope {
    pub principal: String,
    pub workspace: String,
}

impl ReaderScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// What the room can honestly observe about a visit.
///
/// Coarse by construction. The user-agent class tells a phone from a laptop and
/// deliberately nothing more, and dwell is whatever the client chose to report
/// — best effort, `None` the honest common case, and no decision anywhere rests
/// on it. There are no pixels and no beacons in this design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisitContext {
    pub user_agent_class: UserAgentClass,
    pub dwell_ms: Option<u64>,
}

impl VisitContext {
    pub fn unknown() -> Self {
        Self {
            user_agent_class: UserAgentClass::Unknown,
            dwell_ms: None,
        }
    }

    pub fn from_request(req: &HttpRequest, dwell_ms: Option<u64>) -> Self {
        Self {
            user_agent_class: classify_user_agent(
                req.headers()
                    .get(actix_web::http::header::USER_AGENT)
                    .and_then(|value| value.to_str().ok()),
            ),
            dwell_ms,
        }
    }
}

/// A reader asking for the room itself.
///
/// **No `Debug`.** The secret lives in here, and a derived `Debug` is how a
/// working credential ends up in a log line; the manual one redacts it.
#[derive(Clone)]
pub struct RoomRequest {
    pub scope: ReaderScope,
    pub room_id: String,
    pub secret: String,
    pub visit: VisitContext,
}

impl std::fmt::Debug for RoomRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoomRequest")
            .field("scope", &self.scope)
            .field("room_id", &self.room_id)
            .field("secret", &"<redacted>")
            .field("visit", &self.visit)
            .finish()
    }
}

/// A reader asking for one document. `Debug` is redacted for the same reason.
#[derive(Clone)]
pub struct DocumentRequest {
    pub scope: ReaderScope,
    pub room_id: String,
    pub secret: String,
    pub document_ref: String,
    pub visit: VisitContext,
}

impl std::fmt::Debug for DocumentRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DocumentRequest")
            .field("scope", &self.scope)
            .field("room_id", &self.room_id)
            .field("secret", &"<redacted>")
            .field("document_ref", &self.document_ref)
            .field("visit", &self.visit)
            .finish()
    }
}

// ── What a reader is told ───────────────────────────────────────────────────

/// One document a reader may open. A **reference**, never a copy: the point of
/// a room is that what a counterparty sees is what we have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReaderDocument {
    pub reference: String,
    pub added_at: DateTime<Utc>,
}

/// The room as one reader sees it.
///
/// `documents` is that identity's listing and nobody else's. There is no field
/// for who else holds a link, no count of the audience, and no echo of the
/// identity the link was issued to: a forwarded link would otherwise disclose
/// an audience member's address to whoever it was forwarded to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoomIndex {
    pub room_id: String,
    /// Served on the room itself, never buried in a policy — the phase 3
    /// acceptance criterion, which nothing could meet until this surface
    /// existed.
    pub notice: &'static str,
    /// Which opening of the room this is: first, or nth. A count, not a rate,
    /// and the same number the room's own `visits` is counted from — never the
    /// number of times the credential has been presented.
    pub visit: u32,
    pub documents: Vec<ReaderDocument>,
}

/// One document, served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServedDocument {
    pub room_id: String,
    pub notice: &'static str,
    /// The opening this fetch belongs to — the number of the room opening it
    /// followed, not a number of its own. Fetching three documents under one
    /// opening reports the same visit three times.
    pub visit: u32,
    pub document: ReaderDocument,
}

// ── Why a reader is refused ─────────────────────────────────────────────────

/// Every way this surface says no.
///
/// One variant per reason because the **reader** needs to know whether to
/// retry, ask a person, or stop — *"this ended, ask the sender"* and *"this was
/// never a link"* call for different behaviour, and collapsing them is how a
/// counterparty spends a week believing a URL is broken.
///
/// The refusals deliberately do **not** distinguish a room that does not exist
/// from a secret that names no credential, or a document a reader may not see
/// from one that is not there: either distinction is an oracle that answers
/// questions about a room to somebody who was just refused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderRefusal {
    /// The request could not be a room link at all: no secret, a blank or
    /// separator-carrying component, or a room id that is not a derived one.
    MalformedRequest,
    /// No room, or no current credential carries this secret. One answer for
    /// both, on purpose.
    UnknownLink,
    /// The owner killed the credential.
    LinkRevoked,
    /// Past its expiry — inclusively.
    LinkExpired,
    /// The credential is bound to a different relationship than the room.
    AudienceMismatch,
    /// The relationship the room derives from ran, and has ended.
    RelationshipEnded,
    /// The relationship the room derives from was **never recorded**, so there
    /// is nothing for the room's access to derive from.
    ///
    /// Kept apart from [`Self::RelationshipEnded`] because they are opposite
    /// facts and only one of them is about something that happened. A reader
    /// told a relationship ended goes back to whoever sent the link and asks
    /// about the deal; a reader told it was never set up needs the sender to go
    /// and record it. Collapsing the two is how a counterparty and an owner
    /// spend a week each believing the other withdrew something.
    ///
    /// Decided from the roster, which is read before the credential — but
    /// **said only to a caller whose secret named a link on the room**, like
    /// every other refusal the roster decides. This is the sharpest of them:
    /// it reports the *state* of a relationship, and its twin
    /// [`Self::RelationshipEnded`] can only ever come back from the credential
    /// check itself. Left in front of that check, the two halves of one
    /// distinction sat on opposite sides of it, and a stranger who could name
    /// a room id learned a fact about a relationship they had proved no
    /// connection to. To such a caller this is [`Self::UnknownLink`], exactly
    /// as an unknown room is — see
    /// `ReaderSurface::only_to_a_credential_holder`.
    RelationshipNotStarted,
    /// The roster no longer names the identity the link was issued to.
    IdentityNotAdmitted,
    /// The roster could not be established at all. Unknown is never permission.
    ///
    /// Roster-decided, so like [`Self::RelationshipNotStarted`] it is said only
    /// to a caller whose secret named a link on the room.
    RelationshipUnknown,
    /// The owner closed the room.
    RoomClosed,
    /// The room reached its own closing date.
    RoomExpired,
    /// Not in this reader's listing: absent, withdrawn, or restricted away.
    DocumentNotAvailable,
}

impl ReaderRefusal {
    /// The stable machine-readable reason.
    pub fn code(self) -> &'static str {
        match self {
            Self::MalformedRequest => "malformed_request",
            Self::UnknownLink => "unknown_link",
            Self::LinkRevoked => "link_revoked",
            Self::LinkExpired => "link_expired",
            Self::AudienceMismatch => "audience_mismatch",
            Self::RelationshipEnded => "relationship_ended",
            Self::RelationshipNotStarted => "relationship_not_started",
            Self::IdentityNotAdmitted => "identity_not_admitted",
            Self::RelationshipUnknown => "relationship_unknown",
            Self::RoomClosed => "room_closed",
            Self::RoomExpired => "room_expired",
            Self::DocumentNotAvailable => "document_not_available",
        }
    }

    /// `410 Gone` means **it ended** — the link, the room or the relationship
    /// ran out — and is the status that tells a reader to ask rather than
    /// retry. `403` means it is not theirs, or the relationship behind it
    /// cannot be established at all; a relationship that was never recorded
    /// belongs here and deliberately **not** under `410`, because `410` is a
    /// claim that something ran and stopped. `404` means there is nothing here
    /// to talk about. A lapsed room is therefore distinguishable from a broken
    /// URL by status alone, without reading the body.
    pub fn status(self) -> StatusCode {
        match self {
            Self::MalformedRequest => StatusCode::BAD_REQUEST,
            Self::UnknownLink | Self::DocumentNotAvailable => StatusCode::NOT_FOUND,
            Self::LinkRevoked
            | Self::AudienceMismatch
            | Self::IdentityNotAdmitted
            | Self::RelationshipUnknown
            | Self::RelationshipNotStarted => StatusCode::FORBIDDEN,
            Self::LinkExpired | Self::RelationshipEnded | Self::RoomClosed | Self::RoomExpired => {
                StatusCode::GONE
            },
        }
    }

    /// Said plainly, to somebody outside the workspace who cannot read a log.
    pub fn message(self) -> &'static str {
        match self {
            Self::MalformedRequest => {
                "This is not a well-formed room link. Check the whole address was copied."
            },
            Self::UnknownLink => {
                "This link does not open a room. It may have been replaced — ask the sender \
                 for a current one."
            },
            Self::LinkRevoked => "This link has been withdrawn. Ask the sender for a new one.",
            Self::LinkExpired => "This link has expired. Ask the sender for a new one.",
            Self::AudienceMismatch => {
                "This link is not for this room. Ask the sender for the right one."
            },
            Self::RelationshipEnded => {
                "The relationship this room belongs to has ended, so the room is no longer \
                 open. Ask the sender if you still need the documents."
            },
            Self::RelationshipNotStarted => {
                "The relationship this room belongs to has not been set up yet, so there is \
                 nothing for it to open. Nothing has been withdrawn from you — ask the sender \
                 to record the relationship and send the link again."
            },
            Self::IdentityNotAdmitted => {
                "This link was issued to someone who is no longer part of this relationship. \
                 Ask the sender for a link of your own."
            },
            Self::RelationshipUnknown => {
                "The relationship this room belongs to could not be established, so nothing \
                 is being served. Ask the sender to check the room."
            },
            Self::RoomClosed => {
                "This room has been closed. Ask the sender to reopen it if you still need the \
                 documents."
            },
            Self::RoomExpired => {
                "This room has reached its closing date. Ask the sender to reopen it if you \
                 still need the documents."
            },
            Self::DocumentNotAvailable => "That document is not available in this room.",
        }
    }
}

/// The grant layer's refusal, as this surface's.
///
/// A straight mapping and nothing more: every gate keeps its own honest reason,
/// because an audit that reads "revoked" for a string that was never a
/// credential describes an act the owner never performed.
fn refusal_from(refused: PresentationRefused) -> ReaderRefusal {
    match refused {
        PresentationRefused::UnknownSecret => ReaderRefusal::UnknownLink,
        PresentationRefused::Revoked => ReaderRefusal::LinkRevoked,
        PresentationRefused::Expired => ReaderRefusal::LinkExpired,
        PresentationRefused::AudienceMismatch => ReaderRefusal::AudienceMismatch,
        PresentationRefused::AudienceEnded => ReaderRefusal::RelationshipEnded,
        PresentationRefused::IdentityNotAdmitted => ReaderRefusal::IdentityNotAdmitted,
    }
}

/// A room's own clock, as a refusal.
fn refusal_from_standing(standing: RoomStanding) -> Option<ReaderRefusal> {
    match standing {
        RoomStanding::Open => None,
        RoomStanding::Closed => Some(ReaderRefusal::RoomClosed),
        RoomStanding::Expired => Some(ReaderRefusal::RoomExpired),
    }
}

// ── The surface ─────────────────────────────────────────────────────────────

/// A reader who got through every gate: the room, the roster it was checked
/// against, and the numbered presentation that admitted them.
struct Admitted {
    room: DataRoom,
    audience: Audience,
    presentation: Presentation,
}

/// Whether a presentation opens a visit or joins the one it follows.
///
/// The distinction is the **request**, never the row that gets written: a
/// refused document fetch is recorded with no `document_ref`, exactly as an
/// index view is, and numbering it as an opening would report a reader guessing
/// at references as a reader who kept coming back.
#[derive(Debug, Clone, Copy)]
enum VisitBoundary {
    /// `GET /rooms/{room_id}` — the reader opened the room, which is what a
    /// visit is here. Takes the token's next number.
    OpensTheRoom,
    /// A document fetch. It carries the number of the opening it follows, and
    /// opens the first visit itself when the token has no recorded event.
    UnderAnOpenRoom,
}

/// The decision layer. The HTTP handlers below do extraction and status
/// mapping and nothing else, so every refusal in this file is testable without
/// a server.
pub struct ReaderSurface {
    rooms: DataRoomStore,
    links: ShareLinkStore,
    access: AccessStore,
    audiences: Arc<dyn AudienceSource>,
}

impl ReaderSurface {
    pub fn new(workspace: ArtifactV2Workspace, audiences: Arc<dyn AudienceSource>) -> Self {
        Self {
            rooms: DataRoomStore::new(workspace.clone()),
            links: ShareLinkStore::new(workspace.clone()),
            access: AccessStore::new(workspace),
            audiences,
        }
    }

    /// Serve the room to whoever holds this secret.
    ///
    /// The outer `Result` is infrastructure — a store that could not be read or
    /// written. The inner one is the decision. They are kept apart because a
    /// disk fault is not a refusal: telling a reader their link was revoked
    /// because a log was unreadable is a claim about what the owner did.
    pub async fn open_room(
        &self,
        request: &RoomRequest,
        now: DateTime<Utc>,
    ) -> Result<Result<RoomIndex, ReaderRefusal>> {
        let admitted = match self
            .admit(&request.scope, &request.room_id, &request.secret, now)
            .await?
        {
            Ok(admitted) => admitted,
            Err(refusal) => return Ok(Err(refusal)),
        };

        // Decide first, record second, serve third. The decision reads no file
        // and discloses nothing, so ordering it ahead of the record costs
        // nothing and lets the record say what actually happened.
        let refusal = refusal_from_standing(admitted.room.standing(now));

        // Record BEFORE the body, and on the refusal path too: a reader who
        // was turned away at the room's own clock still came, and a room that
        // served nothing is still a room somebody opened.
        let opening = self.record_visit(
            &request.scope,
            &admitted,
            None,
            request.visit,
            VisitBoundary::OpensTheRoom,
            now,
        )?;

        if let Some(refusal) = refusal {
            return Ok(Err(refusal));
        }

        // Phase 5, enforced: this identity's listing, and never a document it
        // may not open. `visible_to` re-checks both clocks and the roster, so
        // the enforcement is the room's rule rather than a copy of it.
        let documents = admitted
            .room
            .visible_to(&admitted.presentation.issued_to, &admitted.audience, now)
            .into_iter()
            .map(|entry| ReaderDocument {
                reference: entry.artifact_ref.clone(),
                added_at: entry.added_at,
            })
            .collect();

        Ok(Ok(RoomIndex {
            room_id: admitted.room.room_id.clone(),
            notice: ROOM_LOGGING_NOTICE,
            visit: opening,
            documents,
        }))
    }

    /// Serve one document under the same gates: present, record, permit, serve.
    ///
    /// The permit check is [`DataRoom::visible_to`] again rather than a
    /// document-level shortcut, so a document a reader cannot see in the
    /// listing is a document they cannot fetch by guessing its reference — and
    /// the refusal is the same one an absent document gets, because a reader
    /// who was just told "not for you" has learned that it exists.
    pub async fn open_document(
        &self,
        request: &DocumentRequest,
        now: DateTime<Utc>,
    ) -> Result<Result<ServedDocument, ReaderRefusal>> {
        if let Err(refusal) = guard_reference(&request.document_ref) {
            return Ok(Err(refusal));
        }
        let admitted = match self
            .admit(&request.scope, &request.room_id, &request.secret, now)
            .await?
        {
            Ok(admitted) => admitted,
            Err(refusal) => return Ok(Err(refusal)),
        };

        let permitted = admitted
            .room
            .visible_to(&admitted.presentation.issued_to, &admitted.audience, now)
            .into_iter()
            .find(|entry| entry.artifact_ref == request.document_ref)
            .map(|entry| ReaderDocument {
                reference: entry.artifact_ref.clone(),
                added_at: entry.added_at,
            });

        let decision = match refusal_from_standing(admitted.room.standing(now)) {
            Some(refusal) => Err(refusal),
            None => permitted.ok_or(ReaderRefusal::DocumentNotAvailable),
        };

        // The recorded reference names the document ONLY when it is about to
        // be served. Recording it on the refusal path would write "they opened
        // this" about a reader who was turned away, and that row is what
        // `documents_opened` and the partial-read signal are built from.
        let served_reference = decision
            .as_ref()
            .ok()
            .map(|document| document.reference.clone());
        let opening = self.record_visit(
            &request.scope,
            &admitted,
            served_reference.as_deref(),
            request.visit,
            VisitBoundary::UnderAnOpenRoom,
            now,
        )?;

        Ok(decision.map(|document| ServedDocument {
            room_id: admitted.room.room_id.clone(),
            notice: ROOM_LOGGING_NOTICE,
            visit: opening,
            document,
        }))
    }

    /// Shape, room, roster, credential — in that order, and every one of them
    /// can end the request.
    ///
    /// The roster is read before the credential because the credential is
    /// checked *against* it, and that ordering is not negotiable. What the
    /// roster **decided**, though, is disclosed only to a caller whose secret
    /// named a link on this room: see `only_to_a_credential_holder`, which is
    /// what keeps this order from being an oracle a stranger can walk.
    async fn admit(
        &self,
        scope: &ReaderScope,
        room_id: &str,
        secret: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<Admitted, ReaderRefusal>> {
        if let Err(refusal) = guard_scope(scope) {
            return Ok(Err(refusal));
        }
        if secret.trim().is_empty() {
            // A blank secret could never name a credential — the grant layer
            // refuses to mint one — so this only ever saves a pointless fold.
            // It is here because refusing before `present` also means refusing
            // before a presentation is spent.
            return Ok(Err(ReaderRefusal::MalformedRequest));
        }
        if !is_derived_room_id(room_id) {
            // Not merely fail-closed: `DataRoomStore` interpolates this id into
            // a file name, so an id shaped like a path is refused before it
            // ever reaches the store. Answering `UnknownLink` rather than
            // `MalformedRequest` keeps the one answer that unknown rooms get.
            return Ok(Err(ReaderRefusal::UnknownLink));
        }

        let room_scope = DataRoomScope::new(scope.principal.as_str(), scope.workspace.as_str());
        let Some(room) = self.rooms.load(&room_scope, room_id)? else {
            // A room nobody opened and a secret nobody issued get the same
            // answer: otherwise the surface tells a stranger which room ids
            // are real.
            return Ok(Err(ReaderRefusal::UnknownLink));
        };

        let link_scope = ShareLinkScope::new(scope.principal.as_str(), scope.workspace.as_str());

        // Two refusals, not one. A room whose counterparty nobody holds and a
        // room whose counterparty was never engaged both refuse and neither
        // opens anything, but they send the sender to different places.
        //
        // Both go through `only_to_a_credential_holder`, so the party who is
        // told which of *ask a person* and *record the relationship* would work
        // is a party who holds a link on this room. To anybody else the room's
        // roster is none of their business and the answer is `UnknownLink`.
        let audience = match self
            .audiences
            .living_audience(&scope.principal, &scope.workspace, &room.audience, now)
            .await?
        {
            AudienceStanding::Roster(audience) => audience,
            AudienceStanding::NeverEngaged => {
                return Ok(Err(self.only_to_a_credential_holder(
                    &link_scope,
                    &room,
                    secret,
                    ReaderRefusal::RelationshipNotStarted,
                    now,
                )?))
            },
            AudienceStanding::Unknown => {
                return Ok(Err(self.only_to_a_credential_holder(
                    &link_scope,
                    &room,
                    secret,
                    ReaderRefusal::RelationshipUnknown,
                    now,
                )?))
            },
        };
        if audience.reference != room.audience {
            // The roster that came back is for a different relationship — a
            // merged-away organisation, or a source answering loosely. Refusing
            // here is what stops `visible_to` from returning an empty listing
            // for that reason and this surface from serving the empty room as
            // if it were the truth.
            //
            // Deliberately NOT handed to `present` as-is to let it decide: a
            // link bound to the relationship the roster resolved *to* would
            // pass every gate, and the room would open under the surviving
            // organisation. A room pointed at a folded-away id has to be
            // re-pointed by its owner, never silently re-aimed.
            return Ok(Err(self.only_to_a_credential_holder(
                &link_scope,
                &room,
                secret,
                ReaderRefusal::AudienceMismatch,
                now,
            )?));
        }
        // The room id is the resource the grants are issued against. The
        // presentation is checked against the LIVING roster, so revoking the
        // relationship closes the room without touching a single link.
        match self
            .links
            .present(&link_scope, &room.room_id, secret, &audience, now)?
        {
            Ok(presentation) => Ok(Ok(Admitted {
                room,
                audience,
                presentation,
            })),
            Err(refused) => Ok(Err(refusal_from(refused))),
        }
    }

    /// A roster refusal, said **only** to a caller whose secret named a link
    /// on this room. To everybody else: [`ReaderRefusal::UnknownLink`].
    ///
    /// The refusals above the credential check are not all the same kind of
    /// disclosure. `MalformedRequest` gives nothing back beyond a judgement on
    /// what the caller typed, and the unknown-room `UnknownLink` is deliberately
    /// the same answer a wrong secret gets, so it separates nothing. A roster
    /// refusal is different: it reports the state of a relationship — that
    /// nobody ever recorded
    /// one, that the counterparty is not on file, that the room points at an
    /// organisation that has been folded away — to somebody who has offered no
    /// evidence of any connection to it. Reported straight, that is an oracle:
    /// a 32-hex-digit room id and any string at all would tell a stranger the
    /// id is real *and* what is wrong behind it, while a healthy room answered
    /// `unknown_link` and gave nothing away. The module already promises those
    /// two are indistinguishable; this is what makes the promise true for a
    /// room whose relationship is in a bad state as well as a healthy one.
    ///
    /// It matters most for `RelationshipNotStarted`, whose twin
    /// `RelationshipEnded` can only arrive from [`ShareLinkStore::present`] —
    /// after the secret matched. Left in front of the check, the two halves of
    /// one distinction sat on opposite sides of it.
    ///
    /// # Why this asks `present` rather than comparing a hash of its own
    ///
    /// A second copy of that gate is the thing that would one day disagree
    /// with it, and the secret's hashing is the store's business alone — the
    /// plaintext must not grow a second consumer. So the question is put to
    /// `present`, and exactly **one bit** of its answer is read: whether the
    /// secret named a link. That is `present`'s first gate and it runs before
    /// the audience is touched at all, so the roster handed in below cannot
    /// colour it. Every other refusal it can return is discarded rather than
    /// reported — those were decided against a roster this function made up,
    /// and reporting one would report a fabricated fact.
    ///
    /// # Why this can never admit anybody, and never spends a presentation
    ///
    /// The roster handed in names nobody and ends at `now`, which fails closed
    /// twice over: `Audience::is_current` is false at the instant it is asked
    /// (expiry is inclusive here) and `Audience::admits` is an `any` over an
    /// empty list. `present` appends its `Presented` record only after every
    /// gate has passed, so no row reaches the log on this path and no visit is
    /// numbered off one.
    fn only_to_a_credential_holder(
        &self,
        scope: &ShareLinkScope,
        room: &DataRoom,
        secret: &str,
        refusal: ReaderRefusal,
        now: DateTime<Utc>,
    ) -> Result<ReaderRefusal> {
        let nobody = Audience::new(room.audience.clone(), Vec::new()).expiring_at(now);
        Ok(
            match self
                .links
                .present(scope, &room.room_id, secret, &nobody, now)?
            {
                // No link on this room carries this secret. The caller has
                // proved no connection to the room, so they get what a room
                // that does not exist gets.
                Err(PresentationRefused::UnknownSecret) => ReaderRefusal::UnknownLink,
                // The secret DID name a link, whatever else is wrong with it —
                // revoked, lapsed, bound elsewhere. That is possession, which
                // is all this surface ever asks for, so the roster's own reason
                // is theirs to hear.
                Err(_) => refusal,
                // Unreachable: the roster above admits nobody and has already
                // ended. Answering with the refusal keeps the impossible case
                // on the fail-closed side rather than turning a surprise into
                // an admission.
                Ok(_) => refusal,
            },
        )
    }

    /// One presentation, one event, written before anything is served —
    /// numbered by the visit it belongs to, and that number returned.
    ///
    /// The event carries `token_issued_to` and no identity field: a capability
    /// URL proves possession of a link, links get forwarded, and an associate
    /// forwarding a room to a partner is ordinary behaviour the log must not
    /// describe as somebody opening a document.
    ///
    /// A failure here fails the whole request, and the visit goes unrecorded:
    /// the grant slot has spent a presentation, but the lane's numbering does
    /// not come from there, so the token's next opening takes the number this
    /// one would have had. That under-reports a visit that served nothing and
    /// never over-reports one — and it never serves a byte.
    fn record_visit(
        &self,
        scope: &ReaderScope,
        admitted: &Admitted,
        document_ref: Option<&str>,
        visit: VisitContext,
        boundary: VisitBoundary,
        now: DateTime<Utc>,
    ) -> Result<u32> {
        let sequence = self.visit_number(
            scope,
            &admitted.room.room_id,
            &admitted.presentation.issued_to,
            boundary,
        )?;
        let event = AccessEvent {
            room_id: admitted.room.room_id.clone(),
            audience: admitted.room.audience.clone(),
            token_issued_to: admitted.presentation.issued_to.clone(),
            document_ref: document_ref.map(str::to_string),
            occurred_at: now,
            dwell_ms: visit.dwell_ms,
            sequence,
            user_agent_class: visit.user_agent_class,
        };
        self.access.record_access(
            &AccessScope::new(scope.principal.as_str(), scope.workspace.as_str()),
            &admitted.room.room_id,
            &event,
        )?;
        Ok(sequence)
    }

    /// Which visit this presentation belongs to.
    ///
    /// Read back from the lane rather than taken from
    /// [`Presentation::sequence`], which counts the grant slot's presentations
    /// and would make every fetch a visit of its own. `record_access` folds the
    /// same log for its idempotency check, so this reads a file that was going
    /// to be read either way.
    ///
    /// An unreadable lane is an error and never a zero — `magician_v2::jsonl`
    /// keeps "absent" and "unreadable" apart, and folding a fault to "no
    /// events" would restart a token's numbering at one and report a
    /// thoroughly read room as barely opened. Only a lane with nothing in it
    /// yields nothing.
    ///
    /// Concurrency is the one place this is imprecise: two openings that both
    /// read the highest number before either has written land on the same
    /// number and read as one visit. That under-reports a return, never invents
    /// one, and cannot reach `NeverOpened` — which is the case where no event
    /// exists at all.
    fn visit_number(
        &self,
        scope: &ReaderScope,
        room_id: &str,
        token_issued_to: &str,
        boundary: VisitBoundary,
    ) -> Result<u32> {
        let highest = self
            .access
            .events_for(
                &AccessScope::new(scope.principal.as_str(), scope.workspace.as_str()),
                room_id,
            )?
            .into_iter()
            .filter(|event| event.token_issued_to == token_issued_to)
            .map(|event| event.sequence)
            .max()
            .unwrap_or(0);
        Ok(match boundary {
            VisitBoundary::OpensTheRoom => highest.saturating_add(1),
            // Never zero: the lane refuses a sequence of zero, and a token
            // whose first recorded act is a document fetch is a token opening
            // its first visit with that fetch.
            VisitBoundary::UnderAnOpenRoom => highest.max(1),
        })
    }
}

/// A scope carrying the separator is refused before anything derives an id from
/// it: `U+001F` is what keeps a presentation id's components from bleeding into
/// each other, and a principal carrying it could fuse two tenants' access into
/// one row. The stores refuse it too — this refuses it *earlier*, before a
/// presentation has been spent on a request that cannot be recorded.
fn guard_scope(scope: &ReaderScope) -> Result<(), ReaderRefusal> {
    if scope.principal.trim().is_empty() || scope.workspace.trim().is_empty() {
        return Err(ReaderRefusal::MalformedRequest);
    }
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        return Err(ReaderRefusal::MalformedRequest);
    }
    Ok(())
}

/// The same rule for a document reference, which is also an id component.
fn guard_reference(reference: &str) -> Result<(), ReaderRefusal> {
    if reference.trim().is_empty() || reference.contains(FIELD_SEP) {
        return Err(ReaderRefusal::MalformedRequest);
    }
    Ok(())
}

/// Whether a room id is one [`DataRoomStore`] could have derived.
///
/// `room-` plus 32 lowercase hex digits, exactly. Anything else never reaches
/// the store, which interpolates the id into a path.
pub fn is_derived_room_id(room_id: &str) -> bool {
    let Some(digits) = room_id.strip_prefix(ROOM_ID_PREFIX) else {
        return false;
    };
    digits.len() == ROOM_ID_DIGITS
        && digits
            .chars()
            .all(|digit| digit.is_ascii_digit() || ('a'..='f').contains(&digit))
}

/// Phone or laptop, and deliberately nothing finer.
///
/// The raw string is classified and thrown away — it is never stored — because
/// the room is observable in the way a place you visit is observable, and a
/// fingerprint is a different thing entirely. An absent or unreadable header is
/// `Unknown`, never a guess.
pub fn classify_user_agent(raw: Option<&str>) -> UserAgentClass {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return UserAgentClass::Unknown;
    };
    let lowered = raw.to_ascii_lowercase();
    if ["mobi", "android", "iphone", "ipad", "ipod"]
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        UserAgentClass::Mobile
    } else {
        UserAgentClass::Desktop
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RoomQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// The capability secret, when it could not travel as a header.
    #[serde(default)]
    pub k: Option<String>,
    #[serde(default)]
    pub dwell_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct DocumentQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub k: Option<String>,
    #[serde(default)]
    pub dwell_ms: Option<u64>,
    /// The document, as a query parameter rather than a path segment: an
    /// artifact reference may carry slashes and `@revision`, and a path segment
    /// would force every caller to escape it identically or fetch the wrong
    /// thing.
    #[serde(rename = "ref", default)]
    pub reference: Option<String>,
}

/// `GET /rooms/{room_id}` — the room, as this reader sees it.
pub async fn read_room_handler(
    surface: web::Data<ReaderSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<RoomQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let Some(secret) = presented_secret(&req, query.k.as_deref()) else {
        return refusal_response(ReaderRefusal::MalformedRequest);
    };
    let request = RoomRequest {
        scope: ReaderScope::new(principal, workspace),
        room_id: path.into_inner(),
        secret,
        visit: VisitContext::from_request(&req, query.dwell_ms),
    };
    match surface.open_room(&request, Utc::now()).await {
        Ok(Ok(index)) => HttpResponse::Ok().json(index),
        Ok(Err(refusal)) => refusal_response(refusal),
        Err(error) => unavailable(&error),
    }
}

/// `GET /rooms/{room_id}/document?ref=…` — one document, under the same gates.
pub async fn read_room_document_handler(
    surface: web::Data<ReaderSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<DocumentQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let Some(secret) = presented_secret(&req, query.k.as_deref()) else {
        return refusal_response(ReaderRefusal::MalformedRequest);
    };
    let request = DocumentRequest {
        scope: ReaderScope::new(principal, workspace),
        room_id: path.into_inner(),
        secret,
        document_ref: query.reference.clone().unwrap_or_default(),
        visit: VisitContext::from_request(&req, query.dwell_ms),
    };
    match surface.open_document(&request, Utc::now()).await {
        Ok(Ok(document)) => HttpResponse::Ok().json(document),
        Ok(Err(refusal)) => refusal_response(refusal),
        Err(error) => unavailable(&error),
    }
}

/// The header first, the query parameter second. Trimmed, and a blank one is
/// no secret at all.
fn presented_secret(req: &HttpRequest, from_query: Option<&str>) -> Option<String> {
    req.headers()
        .get(READER_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            from_query
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn refusal_response(refusal: ReaderRefusal) -> HttpResponse {
    HttpResponse::build(refusal.status()).json(serde_json::json!({
        "error": refusal.code(),
        "message": refusal.message(),
    }))
}

/// A store that could not be read or written. The reader is told nothing about
/// the fault — an external reader is the last person who should learn a path on
/// our disk — and **no body is served**, because the alternative to an
/// unrecorded visit is not a served document.
fn unavailable(error: &anyhow::Error) -> HttpResponse {
    tracing::error!(error = ?error, "data room reader surface could not complete a request");
    HttpResponse::ServiceUnavailable().json(serde_json::json!({
        "error": "room_unavailable",
        "message": "This room could not be opened right now. Try again shortly.",
    }))
}

/// Mounts the two reader routes.
///
/// Deliberately its own scope so it can be mounted **outside** any middleware
/// that authenticates workspace members: a counterparty holding a capability
/// URL is not a member, and putting these routes behind such a gate would make
/// the whole surface unreachable by exactly the people it exists for.
pub fn configure_data_room_reader_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/rooms")
            .route("/{room_id}", web::get().to(read_room_handler))
            .route(
                "/{room_id}/document",
                web::get().to(read_room_document_handler),
            ),
    );
}

#[cfg(test)]
mod tests {
    //! The surface's contract, as behaviour. Every test names the failure it
    //! pins, and asserts values rather than shapes: a refusal that is merely
    //! "an error" would pass while telling a counterparty the wrong thing.

    use super::*;

    use actix_web::{test as actix_test, App};
    use chrono::{Duration, TimeZone};

    use magician::magician_v2::counterparties::CreateCounterparty;
    use magician::magician_v2::evidence::OutwardAssertionStore;
    use magician::magician_v2::obligations::ObligationScope;
    use magician::magician_v2::share_links::IssueShareLink;
    use magician_learning::data_room::{
        attention_for, attention_notes, snapshot_from_events, AttentionReading, AttentionSignal,
        DocumentVisibility, FollowUpPolicy, GrantDisclosure, OpenDataRoom, SharedRoom, SharedToken,
    };

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";
    const OWNER: &str = "owner@ours.test";
    const READER: &str = "reader@acme.test";
    const NEIGHBOUR: &str = "neighbour@acme.test";
    const READER_SECRET: &str = "reader-capability-secret";
    const NEIGHBOUR_SECRET: &str = "neighbour-capability-secret";
    const DECK: &str = "artifact://deck@3";
    const FINANCIALS: &str = "artifact://financials@1";

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, hour, 0, 0).unwrap()
    }

    fn roster(identities: Vec<String>, ends_at: Option<DateTime<Utc>>) -> Audience {
        let audience = Audience::new(AudienceRef::engagement("acme"), identities);
        match ends_at {
            Some(ends_at) => audience.expiring_at(ends_at),
            None => audience,
        }
    }

    fn both_readers() -> Audience {
        roster(vec![READER.to_string(), NEIGHBOUR.to_string()], None)
    }

    /// A roster source that answers with one fixed audience, and
    /// [`AudienceStanding::Unknown`] for any other relationship.
    struct StaticRoster(Option<Audience>);

    #[async_trait::async_trait]
    impl AudienceSource for StaticRoster {
        async fn living_audience(
            &self,
            _principal: &str,
            _workspace: &str,
            reference: &AudienceRef,
            _now: DateTime<Utc>,
        ) -> Result<AudienceStanding> {
            match self
                .0
                .clone()
                .filter(|audience| audience.reference == *reference)
            {
                Some(audience) => Ok(AudienceStanding::Roster(audience)),
                None => Ok(AudienceStanding::Unknown),
            }
        }
    }

    /// A roster source for a counterparty who is on file and has never been
    /// engaged. The one standing that used to be indistinguishable from an
    /// ending.
    struct NeverEngagedRoster;

    #[async_trait::async_trait]
    impl AudienceSource for NeverEngagedRoster {
        async fn living_audience(
            &self,
            _principal: &str,
            _workspace: &str,
            _reference: &AudienceRef,
            _now: DateTime<Utc>,
        ) -> Result<AudienceStanding> {
            Ok(AudienceStanding::NeverEngaged)
        }
    }

    /// A roster source that answers every question with one relationship's
    /// roster — the shape a merged-away organisation produces, where the
    /// surviving record answers for the folded one.
    struct MergedRoster(Audience);

    #[async_trait::async_trait]
    impl AudienceSource for MergedRoster {
        async fn living_audience(
            &self,
            _principal: &str,
            _workspace: &str,
            _reference: &AudienceRef,
            _now: DateTime<Utc>,
        ) -> Result<AudienceStanding> {
            Ok(AudienceStanding::Roster(self.0.clone()))
        }
    }

    /// A roster source that cannot be read at all.
    struct UnreadableRoster;

    #[async_trait::async_trait]
    impl AudienceSource for UnreadableRoster {
        async fn living_audience(
            &self,
            _principal: &str,
            _workspace: &str,
            _reference: &AudienceRef,
            _now: DateTime<Utc>,
        ) -> Result<AudienceStanding> {
            anyhow::bail!("the roster store could not be read")
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        workspace: ArtifactV2Workspace,
        room_id: String,
        link_expiry: DateTime<Utc>,
    }

    /// A room with two documents — one for everybody in the audience, one
    /// restricted to `READER` — and a live link for each of the two readers.
    fn room_at(anchor: DateTime<Utc>, closes_at: Option<DateTime<Utc>>) -> Fixture {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let scope = DataRoomScope::new(PRINCIPAL, WORKSPACE);
        let rooms = DataRoomStore::new(workspace.clone());
        let assertions = OutwardAssertionStore::new(workspace.clone());
        let audience = both_readers();

        let room = rooms
            .open(
                &scope,
                &OpenDataRoom {
                    audience: audience.reference.clone(),
                    opened_by: OWNER.to_string(),
                    closes_at,
                },
                anchor,
            )
            .expect("open the room");
        let grant = GrantDisclosure {
            assertions: &assertions,
            audience: &audience,
            holders: &[],
            disclosed_by: OWNER,
        };
        rooms
            .add_document(
                &scope,
                &room.room_id,
                DECK,
                DocumentVisibility::Everyone,
                OWNER,
                &grant,
                anchor,
            )
            .expect("add the deck");
        rooms
            .add_document(
                &scope,
                &room.room_id,
                FINANCIALS,
                DocumentVisibility::Identities {
                    identities: vec![READER.to_string()],
                },
                OWNER,
                &grant,
                anchor,
            )
            .expect("add the financials");

        let link_expiry = anchor + Duration::hours(11);
        let links = ShareLinkStore::new(workspace.clone());
        for (issued_to, secret) in [(READER, READER_SECRET), (NEIGHBOUR, NEIGHBOUR_SECRET)] {
            links
                .issue(
                    &ShareLinkScope::new(PRINCIPAL, WORKSPACE),
                    &IssueShareLink {
                        resource_ref: room.room_id.clone(),
                        audience: audience.reference.clone(),
                        issued_to: issued_to.to_string(),
                        secret: secret.to_string(),
                        expires_at: link_expiry,
                    },
                    anchor,
                )
                .expect("issue a link");
        }

        Fixture {
            _tmp: tmp,
            workspace,
            room_id: room.room_id,
            link_expiry,
        }
    }

    fn room(closes_at: Option<DateTime<Utc>>) -> Fixture {
        room_at(at(9), closes_at)
    }

    impl Fixture {
        fn surface_with(&self, audience: Option<Audience>) -> ReaderSurface {
            ReaderSurface::new(self.workspace.clone(), Arc::new(StaticRoster(audience)))
        }

        fn surface(&self) -> ReaderSurface {
            self.surface_with(Some(both_readers()))
        }

        fn request(&self, secret: &str) -> RoomRequest {
            RoomRequest {
                scope: ReaderScope::new(PRINCIPAL, WORKSPACE),
                room_id: self.room_id.clone(),
                secret: secret.to_string(),
                visit: VisitContext::unknown(),
            }
        }

        fn document(&self, secret: &str, reference: &str) -> DocumentRequest {
            DocumentRequest {
                scope: ReaderScope::new(PRINCIPAL, WORKSPACE),
                room_id: self.room_id.clone(),
                secret: secret.to_string(),
                document_ref: reference.to_string(),
                visit: VisitContext::unknown(),
            }
        }

        fn events(&self) -> Vec<AccessEvent> {
            AccessStore::new(self.workspace.clone())
                .events_for(&AccessScope::new(PRINCIPAL, WORKSPACE), &self.room_id)
                .expect("read the access lane")
        }

        fn references(index: &RoomIndex) -> Vec<String> {
            index
                .documents
                .iter()
                .map(|document| document.reference.clone())
                .collect()
        }
    }

    /// Revocation is what an owner reaches for when a link has gone somewhere
    /// it should not have. If a withdrawn credential still opened the room the
    /// control would be decorative — and a refused presentation must leave no
    /// access row, because a visit that saw nothing is not a visit.
    #[actix_web::test]
    async fn a_withdrawn_link_refuses_and_records_nothing() {
        let fixture = room(None);
        ShareLinkStore::new(fixture.workspace.clone())
            .revoke(
                &ShareLinkScope::new(PRINCIPAL, WORKSPACE),
                &fixture.room_id,
                READER,
                at(10),
            )
            .expect("revoke the link");

        let refusal = fixture
            .surface()
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect_err("a withdrawn link opens nothing");

        assert_eq!(refusal, ReaderRefusal::LinkRevoked);
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
        assert!(
            fixture.events().is_empty(),
            "a refused presentation disclosed nothing, so there is nothing to account for"
        );
    }

    /// Expiry is inclusive, everywhere in this codebase: expiring at eight
    /// means expired at eight. A link that stayed live through the instant it
    /// lapsed is a possession-based grant outliving its own deadline by a
    /// clock tick, which is exactly the standing-yes the mandatory expiry
    /// exists to prevent.
    #[actix_web::test]
    async fn a_link_is_expired_at_the_instant_it_expires_not_after_it() {
        let fixture = room(None);
        let surface = fixture.surface();

        let live = surface
            .open_room(
                &fixture.request(READER_SECRET),
                fixture.link_expiry - Duration::seconds(1),
            )
            .await
            .expect("storage is healthy")
            .expect("a link is live until its expiry");
        assert_eq!(live.visit, 1);

        let refusal = surface
            .open_room(&fixture.request(READER_SECRET), fixture.link_expiry)
            .await
            .expect("storage is healthy")
            .expect_err("expiry is inclusive");
        assert_eq!(refusal, ReaderRefusal::LinkExpired);
        assert_eq!(refusal.status(), StatusCode::GONE);
    }

    /// Access derives from the relationship and ends with it. Someone dropped
    /// from the roster still holds an unrevoked, unexpired URL, and the roster
    /// is read live at presentation time precisely so that URL stops working
    /// without anyone remembering to revoke it.
    #[actix_web::test]
    async fn an_identity_dropped_from_the_roster_refuses_even_holding_a_live_link() {
        let fixture = room(None);

        let refusal = fixture
            .surface_with(Some(roster(vec![NEIGHBOUR.to_string()], None)))
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect_err("the roster no longer names them");

        assert_eq!(refusal, ReaderRefusal::IdentityNotAdmitted);
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
        assert!(fixture.events().is_empty());
    }

    /// Phase 5, in earnest. A restricted document must be **absent from the
    /// listing** rather than merely unfetchable: naming it would disclose that
    /// it exists, that it is for someone else, and what the next conversation
    /// is about — to a reader who is not allowed to open it.
    #[actix_web::test]
    async fn a_restricted_document_is_absent_from_the_listing_of_a_reader_it_is_not_for() {
        let fixture = room(None);
        let surface = fixture.surface();

        let neighbour = surface
            .open_room(&fixture.request(NEIGHBOUR_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect("the room is open to them");
        assert_eq!(Fixture::references(&neighbour), vec![DECK.to_string()]);

        let reader = surface
            .open_room(&fixture.request(READER_SECRET), at(12))
            .await
            .expect("storage is healthy")
            .expect("the room is open to them");
        assert_eq!(
            Fixture::references(&reader),
            vec![DECK.to_string(), FINANCIALS.to_string()]
        );
    }

    /// Record before act. A room that serves a document without recording the
    /// visit is the unaccountable disclosure the whole plan exists to prevent,
    /// so the event is written first and carries `token_issued_to` — the link
    /// that was used — never a claim about who was at the keyboard.
    #[actix_web::test]
    async fn the_access_event_is_written_before_the_body_is_served() {
        let fixture = room(None);
        let index = fixture
            .surface()
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect("the room is open");

        let events = fixture.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].token_issued_to, READER);
        assert_eq!(events[0].document_ref, None);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[0].occurred_at, at(11));
        assert_eq!(index.visit, 1);
    }

    /// The other half of record-before-act, and the one that matters: when the
    /// lane cannot be written, **nothing is served**. A surface that fell back
    /// to serving would turn a disk fault into exactly the disclosure nobody
    /// can account for.
    #[actix_web::test]
    async fn a_visit_that_cannot_be_recorded_is_never_served() {
        let fixture = room(None);
        let scope_root = fixture.workspace.scope_root(PRINCIPAL, WORKSPACE);
        std::fs::create_dir_all(&scope_root).expect("create the scope root");
        // A file where the lane's directory belongs: every write under it now
        // fails, which is what an unwritable lane looks like.
        std::fs::write(scope_root.join("room_access"), b"not a directory")
            .expect("block the access lane");

        let error = fixture
            .surface()
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect_err("an unwritable lane is a fault, never a refusal and never a service");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("room_access"),
            "the fault names the lane it could not write: {rendered}"
        );
    }

    /// A room whose relationship has lapsed says so **plainly**, with a status
    /// of its own. A `404` would tell the counterparty their link is broken, so
    /// they would retry it; `410 Gone` tells them it ended, so they ask.
    #[actix_web::test]
    async fn a_lapsed_relationship_refuses_with_a_status_a_broken_link_never_gets() {
        let fixture = room(None);

        let refusal = fixture
            .surface_with(Some(roster(
                vec![READER.to_string(), NEIGHBOUR.to_string()],
                Some(at(10)),
            )))
            .open_room(&fixture.request(READER_SECRET), at(10))
            .await
            .expect("storage is healthy")
            .expect_err("the relationship ended, inclusively");

        assert_eq!(refusal, ReaderRefusal::RelationshipEnded);
        assert_eq!(refusal.status(), StatusCode::GONE);
        assert_ne!(refusal.status(), ReaderRefusal::UnknownLink.status());
        assert_ne!(refusal.status(), ReaderRefusal::LinkRevoked.status());
    }

    /// The vacuous answer, refused. A closed room's `visible_to` is empty, so a
    /// surface that simply rendered it would serve `documents: []` — an empty
    /// room and a closed one look identical, and only one of them is worth
    /// asking about. The visit is still recorded: they came, and the log says
    /// so.
    #[actix_web::test]
    async fn a_room_past_its_closing_date_refuses_instead_of_serving_an_empty_listing() {
        let fixture = room(Some(at(12)));

        let refusal = fixture
            .surface()
            .open_room(&fixture.request(READER_SECRET), at(13))
            .await
            .expect("storage is healthy")
            .expect_err("the room's own clock has run out");

        assert_eq!(refusal, ReaderRefusal::RoomExpired);
        assert_eq!(refusal.status(), StatusCode::GONE);

        let events = fixture.events();
        assert_eq!(events.len(), 1, "the visit happened and is on the record");
        assert_eq!(events[0].document_ref, None);
        assert_eq!(events[0].sequence, 1);
    }

    /// Visits are counted from the highest `sequence` the lane holds, so an
    /// opening of the room has to take the next number: first to nth, without
    /// gaps. A surface that reused a number would make three separate returns
    /// read as one, and the count of who came back a fiction.
    #[actix_web::test]
    async fn the_visit_number_advances_by_one_per_opening_of_the_room() {
        let fixture = room(None);
        let surface = fixture.surface();

        for expected in 1u32..=3 {
            let index = surface
                .open_room(&fixture.request(READER_SECRET), at(10 + expected))
                .await
                .expect("storage is healthy")
                .expect("the room is open");
            assert_eq!(index.visit, expected);
        }

        let sequences: Vec<u32> = fixture
            .events()
            .iter()
            .map(|event| event.sequence)
            .collect();
        assert_eq!(sequences, vec![1, 2, 3]);
    }

    /// Five presentations inside one visit must not reach the room's owner as
    /// five openings.
    ///
    /// This surface used to write the grant slot's presentation number
    /// (`Presentation::sequence`) straight into `AccessEvent::sequence`, so
    /// every fetch minted a fresh visit and `TokenAttention::visits` was the
    /// click count. `derive_follow_ups` collapses opened-once and
    /// opened-repeatedly and so never noticed — but `data_room::cycle` is a
    /// second consumer that does not, and it prints the number in a sentence
    /// for the owner. A reader who opened the room once and read what was in it
    /// was reported as having opened it five times and come back.
    #[actix_web::test]
    async fn five_presentations_in_one_visit_reach_the_owner_as_one_visit() {
        let fixture = room(None);
        let surface = fixture.surface();

        surface
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect("the room is open");
        for (hour, reference) in [(12, DECK), (13, FINANCIALS), (14, DECK), (15, FINANCIALS)] {
            surface
                .open_document(&fixture.document(READER_SECRET, reference), at(hour))
                .await
                .expect("storage is healthy")
                .expect("it is in their listing");
        }

        let events = fixture.events();
        assert_eq!(events.len(), 5, "one event per presentation, unchanged");
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<u32>>(),
            vec![1, 1, 1, 1, 1],
            "the opening is the visit, and the four fetches join it"
        );

        let documents = vec![DECK.to_string(), FINANCIALS.to_string()];
        let attention = attention_for(READER, &documents, &events);
        assert_eq!(attention.visits, 1);
        assert_eq!(attention.presentations, 5);
        assert_eq!(attention.signal(), AttentionSignal::OpenedOnce);

        // The owner's side of the same events, assembled exactly as a sweep
        // assembles it: the sentence a cycle reads is the thing that was wrong.
        let tokens = snapshot_from_events(
            &fixture.room_id,
            &[SharedToken::new(READER, at(9))],
            &documents,
            &events,
        )
        .expect("the events are this room's");
        let notes = attention_notes(
            &ObligationScope::new(PRINCIPAL, WORKSPACE),
            &[SharedRoom {
                audience: AudienceRef::engagement("acme"),
                room_id: fixture.room_id.clone(),
                tokens,
            }],
            &FollowUpPolicy::new(Duration::days(3), Duration::days(4)).expect("positive windows"),
            at(15) + Duration::days(5),
        )
        .expect("notes");

        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].visits, 1);
        assert_eq!(notes[0].presentations, 5);
        assert_eq!(notes[0].reading, AttentionReading::ReadThenSilent);
        assert_eq!(
            notes[0].headline,
            format!(
                "{READER} opened room '{}' once and has not replied",
                fixture.room_id
            )
        );
    }

    /// Only opening the room starts a visit; a document fetch joins the opening
    /// it follows.
    ///
    /// With a number minted per request, a reader who opened the room, read the
    /// deck, came back a day later and read the financials was four visits
    /// rather than two — and the gap between "came back" and "read it once" is
    /// the whole of what `AttentionSignal` carries.
    #[actix_web::test]
    async fn a_fetch_joins_the_opening_before_it_while_a_second_opening_advances() {
        let fixture = room(None);
        let surface = fixture.surface();

        let first = surface
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect("the room is open");
        let deck = surface
            .open_document(&fixture.document(READER_SECRET, DECK), at(12))
            .await
            .expect("storage is healthy")
            .expect("it is in their listing");
        let second = surface
            .open_room(&fixture.request(READER_SECRET), at(13))
            .await
            .expect("storage is healthy")
            .expect("the room is open");
        let financials = surface
            .open_document(&fixture.document(READER_SECRET, FINANCIALS), at(14))
            .await
            .expect("storage is healthy")
            .expect("it is in their listing");

        assert_eq!((first.visit, deck.visit), (1, 1));
        assert_eq!((second.visit, financials.visit), (2, 2));

        let events = fixture.events();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<u32>>(),
            vec![1, 1, 2, 2]
        );

        let attention = attention_for(READER, &[DECK.to_string(), FINANCIALS.to_string()], &events);
        assert_eq!(attention.visits, 2);
        assert_eq!(attention.presentations, 4);
        assert_eq!(attention.signal(), AttentionSignal::OpenedRepeatedly);
    }

    /// A token whose first recorded act is a document fetch opens its first
    /// visit with that fetch, and is never written at sequence zero.
    ///
    /// Numbering it from the lane means asking "what is the highest sequence
    /// this token reached", and the answer for a token with no history is
    /// nothing. Recording that as-is would be refused by the lane outright, and
    /// a zero that ever got through reads as `NeverOpened` — the delivery
    /// question, raised about a reader who is holding the document.
    #[actix_web::test]
    async fn a_fetch_by_a_token_with_no_history_opens_the_first_visit_never_visit_zero() {
        let fixture = room(None);

        let served = fixture
            .surface()
            .open_document(&fixture.document(READER_SECRET, DECK), at(11))
            .await
            .expect("storage is healthy")
            .expect("it is in their listing");

        assert_eq!(served.visit, 1);

        let events = fixture.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 1);

        let attention = attention_for(READER, &[DECK.to_string(), FINANCIALS.to_string()], &events);
        assert_eq!(attention.visits, 1);
        assert_eq!(attention.signal(), AttentionSignal::OpenedOnce);
    }

    /// Guessing a reference must not work, and being refused must not be
    /// recorded as reading. Writing `document_ref` on the refusal path would
    /// put *"they opened the financials"* in the audit log about a reader who
    /// was turned away — and `documents_opened` is what the partial-read signal
    /// is built from.
    #[actix_web::test]
    async fn a_document_a_reader_may_not_open_is_refused_and_never_recorded_as_opened() {
        let fixture = room(None);

        let refusal = fixture
            .surface()
            .open_document(&fixture.document(NEIGHBOUR_SECRET, FINANCIALS), at(11))
            .await
            .expect("storage is healthy")
            .expect_err("it is not in their listing");

        assert_eq!(refusal, ReaderRefusal::DocumentNotAvailable);
        assert_eq!(
            refusal.status(),
            ReaderRefusal::UnknownLink.status(),
            "refusing a restricted document exactly as an absent one keeps the surface from \
             confirming that it exists"
        );

        let events = fixture.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].document_ref, None);

        let attention = attention_for(
            NEIGHBOUR,
            &[DECK.to_string(), FINANCIALS.to_string()],
            &events,
        );
        assert_eq!(attention.documents_opened, Vec::<String>::new());
        assert_eq!(attention.visits, 1);
        assert_eq!(attention.index_views, 1);
    }

    /// The served half: a permitted document is served and the row names it,
    /// which is what makes *"opened the deck, never opened the financials"*
    /// answerable at all.
    #[actix_web::test]
    async fn a_permitted_document_is_recorded_by_reference_and_then_served() {
        let fixture = room(None);

        let served = fixture
            .surface()
            .open_document(&fixture.document(READER_SECRET, FINANCIALS), at(11))
            .await
            .expect("storage is healthy")
            .expect("it is in their listing");

        assert_eq!(served.document.reference, FINANCIALS);
        assert_eq!(served.visit, 1);
        assert_eq!(served.notice, ROOM_LOGGING_NOTICE);

        let events = fixture.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].document_ref.as_deref(), Some(FINANCIALS));

        let attention = attention_for(READER, &[DECK.to_string(), FINANCIALS.to_string()], &events);
        assert_eq!(attention.documents_opened, vec![FINANCIALS.to_string()]);
        assert_eq!(attention.documents_unopened, vec![DECK.to_string()]);
    }

    /// A room nobody opened and a secret nobody issued get the **same** answer.
    /// Splitting them would turn the surface into an oracle a stranger could
    /// walk to enumerate which rooms exist.
    #[actix_web::test]
    async fn an_unknown_room_is_refused_exactly_as_an_unknown_secret_is() {
        let fixture = room(None);
        let surface = fixture.surface();

        let wrong_secret = surface
            .open_room(&fixture.request("not-a-real-secret"), at(11))
            .await
            .expect("storage is healthy")
            .expect_err("no credential carries that hash");

        let mut absent = fixture.request(READER_SECRET);
        absent.room_id = format!("room-{}", "0".repeat(32));
        let absent_room = surface
            .open_room(&absent, at(11))
            .await
            .expect("storage is healthy")
            .expect_err("no such room");

        assert_eq!(wrong_secret, ReaderRefusal::UnknownLink);
        assert_eq!(absent_room, wrong_secret);
        assert_eq!(absent_room.status(), StatusCode::NOT_FOUND);
    }

    /// The room store interpolates the id into a file name, so an id shaped
    /// like a path would read somewhere it was never meant to. It is refused on
    /// shape, before the store sees it, and the answer is the one every unknown
    /// room gets.
    #[actix_web::test]
    async fn a_room_id_that_is_not_a_derived_id_never_reaches_the_store() {
        let fixture = room(None);
        let surface = fixture.surface();

        for probe in [
            "../../../../etc/passwd",
            "room-../../secrets",
            "room-NOTHEX0000000000000000000000000",
            "room-abc",
            "",
        ] {
            let mut request = fixture.request(READER_SECRET);
            request.room_id = probe.to_string();
            let refusal = surface
                .open_room(&request, at(11))
                .await
                .expect("storage is healthy")
                .expect_err("not a derived room id");
            assert_eq!(refusal, ReaderRefusal::UnknownLink, "probe: {probe}");
        }
        assert!(fixture.events().is_empty());
    }

    /// `U+001F` separates the components of every derived id in this set, so a
    /// scope carrying one could fuse two tenants' access into a single row. The
    /// stores refuse it too — this refuses it **earlier**, before a
    /// presentation has been spent on a request whose visit could never be
    /// recorded.
    #[actix_web::test]
    async fn a_scope_carrying_the_separator_is_refused_before_a_presentation_is_spent() {
        let fixture = room(None);
        let mut request = fixture.request(READER_SECRET);
        request.scope = ReaderScope::new(format!("anon{FIELD_SEP}ymous"), WORKSPACE);

        let refusal = fixture
            .surface()
            .open_room(&request, at(11))
            .await
            .expect("storage is healthy")
            .expect_err("the separator is refused at the edge");
        assert_eq!(refusal, ReaderRefusal::MalformedRequest);
        assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);

        let held = ShareLinkStore::new(fixture.workspace.clone())
            .for_resource(&ShareLinkScope::new(PRINCIPAL, WORKSPACE), &fixture.room_id)
            .expect("read the grants");
        assert!(
            held.iter().all(|link| link.presentations == 0),
            "no presentation may be spent on a request that was refused on shape"
        );
    }

    /// A document reference is an id component too, and a blank one is
    /// indistinguishable from an index view once it is written down — which is
    /// the difference between *"they looked and opened nothing"* and *"they
    /// opened this"*.
    #[actix_web::test]
    async fn a_blank_or_separator_carrying_document_reference_is_refused() {
        let fixture = room(None);
        let surface = fixture.surface();

        for probe in ["", "   ", "artifact://deck\u{1f}financials"] {
            let refusal = surface
                .open_document(&fixture.document(READER_SECRET, probe), at(11))
                .await
                .expect("storage is healthy")
                .expect_err("not a usable reference");
            assert_eq!(refusal, ReaderRefusal::MalformedRequest, "probe: {probe:?}");
        }
        assert!(fixture.events().is_empty());
    }

    /// Unknown is never permission, and unreadable is never an empty roster.
    /// A source that cannot answer must produce a refusal or a fault — never a
    /// roster that happens to admit nobody quietly, and never a served room.
    #[actix_web::test]
    async fn a_roster_that_cannot_be_established_is_never_permission() {
        let fixture = room(None);

        let unknown = fixture
            .surface_with(None)
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect_err("no such relationship");
        assert_eq!(unknown, ReaderRefusal::RelationshipUnknown);
        assert_eq!(unknown.status(), StatusCode::FORBIDDEN);

        let unreadable = ReaderSurface::new(fixture.workspace.clone(), Arc::new(UnreadableRoster))
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await;
        assert!(
            unreadable.is_err(),
            "an unreadable roster is a fault, not a decision"
        );
        assert!(fixture.events().is_empty());
    }

    /// A relationship that was **never recorded** refuses, and refuses as
    /// itself.
    ///
    /// This used to arrive as `relationship_ended` — `410 Gone`, "ask the
    /// sender if you still need the documents" — which is a claim that
    /// something ran and stopped. Nothing ran. The reader would go back to a
    /// sender who had never withdrawn anything, and the sender's own surface
    /// would tell them to revoke, withdraw or close, none of which can create a
    /// relationship that was never recorded. Both answers refuse; only one of
    /// them describes the world.
    #[actix_web::test]
    async fn a_relationship_that_was_never_recorded_does_not_claim_to_have_ended() {
        let fixture = room(None);

        let refusal = ReaderSurface::new(fixture.workspace.clone(), Arc::new(NeverEngagedRoster))
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect_err("a relationship nothing records opens nothing");

        assert_eq!(
            refusal,
            ReaderRefusal::RelationshipNotStarted,
            "a room that never had a relationship is being reported as one that lost it"
        );
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
        assert_ne!(
            refusal.status(),
            ReaderRefusal::RelationshipEnded.status(),
            "`410` is the status that means it ran and stopped, and this never ran — a \
             reader who cannot read the body must still be able to tell the two apart"
        );
        assert!(
            !refusal.message().contains("ended"),
            "the message still tells the reader something ended: {}",
            refusal.message()
        );
        assert!(
            fixture.events().is_empty(),
            "nothing was admitted, so nothing is on the record"
        );
    }

    /// A roster for a **different** relationship is refused rather than
    /// evaluated. Left to run, `visible_to` would answer with an empty listing
    /// for that reason alone, and the surface would serve *"there is nothing
    /// here"* about a room full of documents — the vacuous truth that looks
    /// exactly like a correct answer.
    #[actix_web::test]
    async fn a_roster_for_another_relationship_refuses_rather_than_serving_an_empty_room() {
        let fixture = room(None);
        let elsewhere = Audience::new(
            AudienceRef::account("acme"),
            vec![READER.to_string(), NEIGHBOUR.to_string()],
        );

        let refusal =
            ReaderSurface::new(fixture.workspace.clone(), Arc::new(MergedRoster(elsewhere)))
                .open_room(&fixture.request(READER_SECRET), at(11))
                .await
                .expect("storage is healthy")
                .expect_err("that roster is not this room's");

        assert_eq!(refusal, ReaderRefusal::AudienceMismatch);
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
        assert!(
            fixture.events().is_empty(),
            "nothing was admitted, so nothing is on the record"
        );
    }

    /// A caller who has proved nothing learns nothing finer than *"this link
    /// does not open anything"*.
    ///
    /// The roster is read before the credential — it has to be, the credential
    /// is checked against it — so every refusal the roster decides was
    /// readable by anybody who could name a room id and type any string at
    /// all. A healthy room answers `unknown_link` to a bogus secret, so any
    /// other answer was itself the disclosure: it told a stranger the room id
    /// was real, and then what was wrong behind it.
    ///
    /// `relationship_not_started` was the sharpest case, because it reports
    /// the *state* of a relationship and its twin `relationship_ended` can
    /// only ever come back from `present` — after the secret matched. The two
    /// halves of one distinction sat on opposite sides of the credential
    /// check, which is why they disagreed about who was allowed to hear them.
    ///
    /// Both halves are asserted here on purpose. Collapsing the stranger's
    /// answer while quietly losing the holder's would satisfy the first half
    /// and undo the distinction this surface exists to make.
    #[actix_web::test]
    async fn a_roster_refusal_is_spoken_only_to_somebody_who_holds_a_credential() {
        let fixture = room(None);
        let folded_away = Audience::new(
            AudienceRef::account("acme"),
            vec![READER.to_string(), NEIGHBOUR.to_string()],
        );
        let unhealthy: Vec<(&str, ReaderSurface, ReaderRefusal)> = vec![
            (
                "a relationship nothing ever recorded",
                ReaderSurface::new(fixture.workspace.clone(), Arc::new(NeverEngagedRoster)),
                ReaderRefusal::RelationshipNotStarted,
            ),
            (
                "a counterparty the register does not hold",
                ReaderSurface::new(fixture.workspace.clone(), Arc::new(StaticRoster(None))),
                ReaderRefusal::RelationshipUnknown,
            ),
            (
                "an organisation that has been folded into another",
                ReaderSurface::new(
                    fixture.workspace.clone(),
                    Arc::new(MergedRoster(folded_away)),
                ),
                ReaderRefusal::AudienceMismatch,
            ),
        ];

        for (situation, surface, expected) in unhealthy {
            let stranger = surface
                .open_room(&fixture.request("not-a-real-secret"), at(11))
                .await
                .expect("storage is healthy")
                .expect_err("no credential carries that hash");
            assert_eq!(
                stranger,
                ReaderRefusal::UnknownLink,
                "a caller who presented no credential was told about {situation}; a room id \
                 is all they supplied, and a healthy room would have answered `unknown_link` \
                 — so this answer is itself an oracle for which room ids are real"
            );
            assert_eq!(
                stranger.status(),
                StatusCode::NOT_FOUND,
                "the status alone told a stranger this room is not the healthy kind"
            );

            let holder = surface
                .open_room(&fixture.request(READER_SECRET), at(11))
                .await
                .expect("storage is healthy")
                .expect_err("an unhealthy relationship opens nothing, credential or not");
            assert_eq!(
                holder, expected,
                "a reader holding a link on this room lost the reason for the refusal \
                 ({situation}), which is the one thing that tells them whether to ask a \
                 person, record something, or wait"
            );
        }

        assert!(
            ShareLinkStore::new(fixture.workspace.clone())
                .for_resource(&ShareLinkScope::new(PRINCIPAL, WORKSPACE), &fixture.room_id)
                .expect("the grant log is readable")
                .iter()
                .all(|link| link.presentations == 0),
            "the possession check spent a presentation; it may only ever refuse, or a \
             reader who was turned away has their grant slot counting openings nobody made"
        );
        assert!(
            fixture.events().is_empty(),
            "nothing was admitted, so nothing belongs on the access lane"
        );
    }

    // ── The second clock: when the relationship itself ends ─────────────────

    /// The organisation both real stores below are keyed on. The engagement
    /// names it by the same label the register recorded, because
    /// `counterparty_id_for` derives from the label rather than storing it.
    const ORGANISATION: &str = "Acme Capital";

    /// A real counterparty register holding one organisation, and a real
    /// engagement roster that starts empty.
    ///
    /// Deliberately NOT one of the mock rosters above: the mocks implement
    /// [`AudienceSource`] themselves and so never exercise
    /// [`CounterpartyAudiences`], which is the only implementation production
    /// ever runs.
    struct Relationship {
        _tmp: tempfile::TempDir,
        workspace: ArtifactV2Workspace,
        counterparty_id: String,
        roster: Arc<EngagementStore>,
    }

    async fn relationship() -> Relationship {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let recorded = CounterpartyStore::new(workspace.clone())
            .record_counterparty(
                &CounterpartyScope::new(PRINCIPAL, WORKSPACE),
                &CreateCounterparty {
                    display_name: ORGANISATION.to_string(),
                    domain: None,
                    stage: None,
                    created_by: OWNER.to_string(),
                },
                at(9),
            )
            .expect("record the organisation");
        let roster = Arc::new(
            EngagementStore::open(tmp.path())
                .await
                .expect("open the engagement roster"),
        );
        Relationship {
            _tmp: tmp,
            workspace,
            counterparty_id: recorded.counterparty_id,
            roster,
        }
    }

    /// The roster out of a standing, or a failure that names which of the
    /// three answers came back instead — an assertion that says "expected
    /// `Some`" cannot tell `Unknown` from `NeverEngaged`, and those are the two
    /// this file now exists to keep apart.
    fn roster_of(standing: AudienceStanding) -> Audience {
        match standing {
            AudienceStanding::Roster(audience) => audience,
            other => panic!("expected a roster for a counterparty on file, got {other:?}"),
        }
    }

    impl Relationship {
        fn source(&self) -> CounterpartyAudiences {
            CounterpartyAudiences::new(
                CounterpartyStore::new(self.workspace.clone()),
                self.roster.clone(),
            )
        }

        fn reference(&self) -> AudienceRef {
            AudienceRef::engagement(self.counterparty_id.clone())
        }

        async fn engage(&self, ends_at: DateTime<Utc>) -> String {
            self.roster
                .create(
                    PRINCIPAL,
                    WORKSPACE,
                    "fundraising",
                    ORGANISATION,
                    Default::default(),
                    Default::default(),
                    at(9).timestamp_millis(),
                    ends_at.timestamp_millis(),
                )
                .await
                .expect("mint the engagement")
                .engagement_id
        }
    }

    /// **The vacuous pass this port exists to close.**
    ///
    /// Nothing ever set `Audience::expires_at`, so `Audience::is_current`
    /// answered `true` for every relationship that had ever existed — and both
    /// second-clock guards that lean on it (`DataRoom::visible_to` and the
    /// owner surface's `relationship_ended`) passed for every room ever opened.
    /// The check ran, and always said yes, which is why nobody noticed. This
    /// test is the assertion that would have caught it: the expiry the
    /// engagement carries has to arrive on the audience.
    #[actix_web::test]
    async fn a_live_engagement_stamps_its_own_end_on_the_audience() {
        let relationship = relationship().await;
        let ends_at = at(17);
        relationship.engage(ends_at).await;

        let audience = roster_of(
            relationship
                .source()
                .living_audience(PRINCIPAL, WORKSPACE, &relationship.reference(), at(10))
                .await
                .expect("the register is healthy"),
        );

        assert_eq!(
            audience.reference,
            relationship.reference(),
            "the register answered for a different relationship, so this test would be \
             asserting about the wrong audience"
        );
        assert_eq!(
            audience.expires_at,
            Some(ends_at),
            "the audience carries no end date, so a counterparty keeps reading for ever"
        );
        assert!(
            audience.is_current(at(10)),
            "a live engagement must not close the room it authorises"
        );
        assert!(
            !audience.is_current(ends_at),
            "expiry is inclusive everywhere in this codebase, so the room closes AT the \
             instant the engagement ends, not after it"
        );
    }

    /// Revoking an engagement is what an owner reaches for when a relationship
    /// ends early. If the audience came back unstamped the room would keep
    /// serving documents to a counterparty whose engagement had been withdrawn
    /// — the exact failure the second clock exists to prevent.
    #[actix_web::test]
    async fn a_revoked_engagement_closes_the_room_rather_than_serving_it() {
        let relationship = relationship().await;
        let engagement_id = relationship.engage(at(17)).await;
        assert!(
            relationship
                .roster
                .revoke(&engagement_id, at(10).timestamp_millis())
                .await
                .expect("the roster is writable"),
            "the engagement was not live to begin with, so this test would pass vacuously"
        );

        let audience = roster_of(
            relationship
                .source()
                .living_audience(PRINCIPAL, WORKSPACE, &relationship.reference(), at(11))
                .await
                .expect("the register is healthy"),
        );

        assert_eq!(
            audience.expires_at,
            Some(at(11)),
            "a revoked engagement must stamp the audience closed at the reading instant; \
             leaving it unstamped would let a counterparty keep reading after the \
             relationship ended"
        );
        assert!(
            !audience.is_current(at(11)),
            "the room stayed open on a relationship that was revoked an hour earlier"
        );
    }

    /// The organisation is on file and there is no engagement with it at all.
    ///
    /// That is not permission: an audience whose relationship nothing records
    /// has no lifetime anybody agreed to, and serving it would be access
    /// derived from nothing. It refuses — but as [`AudienceStanding::NeverEngaged`]
    /// and not as a roster stamped closed at `now`, because the second reads
    /// downstream as a relationship that ended and sends both parties looking
    /// for a withdrawal nobody performed.
    #[actix_web::test]
    async fn a_counterparty_with_no_engagement_at_all_is_never_engaged_not_ended() {
        let relationship = relationship().await;
        assert!(
            relationship
                .roster
                .list(PRINCIPAL, WORKSPACE)
                .await
                .is_empty(),
            "the roster was seeded, so this test would not be testing an absent engagement"
        );

        let standing = relationship
            .source()
            .living_audience(PRINCIPAL, WORKSPACE, &relationship.reference(), at(11))
            .await
            .expect("the register is healthy");

        assert_eq!(
            standing,
            AudienceStanding::NeverEngaged,
            "an organisation nobody has ever been engaged with came back as something else; \
             a roster stamped closed at `now` is what made 'never recorded' arrive downstream \
             as 'ended', and an ending is a fact about something that happened"
        );
    }

    /// The two opposite facts, asserted against each other rather than one at a
    /// time. Nothing else in this file would fail if `NeverEngaged` quietly
    /// went back to being a roster stamped at `now`: both refuse, so no
    /// coverage of *whether the room opens* can tell them apart.
    #[actix_web::test]
    async fn a_relationship_that_ended_and_one_that_never_existed_are_not_the_same_answer() {
        let never = relationship().await;
        let never_standing = never
            .source()
            .living_audience(PRINCIPAL, WORKSPACE, &never.reference(), at(11))
            .await
            .expect("the register is healthy");

        let ended = relationship().await;
        let engagement_id = ended.engage(at(17)).await;
        assert!(
            ended
                .roster
                .revoke(&engagement_id, at(10).timestamp_millis())
                .await
                .expect("the roster is writable"),
            "the engagement was not live to begin with, so the ended half would be vacuous"
        );
        let ended_standing = ended
            .source()
            .living_audience(PRINCIPAL, WORKSPACE, &ended.reference(), at(11))
            .await
            .expect("the register is healthy");

        assert_ne!(
            never_standing, ended_standing,
            "a relationship that was never recorded and one that was revoked answered \
             identically, so every message downstream tells one of the two parties something \
             untrue about what happened"
        );
        assert_eq!(never_standing, AudienceStanding::NeverEngaged);
        assert!(
            !roster_of(ended_standing).is_current(at(11)),
            "the revoked half must still close the room — separating the two facts must not \
             turn either of them into permission"
        );
    }

    /// The gap this change does NOT close, pinned so the comment that claims it
    /// cannot quietly become false.
    ///
    /// No store in this tree records when a programme, an account, a panel or a
    /// person relationship ends, so those four audiences are still unstamped
    /// and their second clock is still vacuous. Only the room's own closing
    /// date and the link's expiry bound them. When a store for one of them
    /// arrives, this test is what will fail and say so.
    #[actix_web::test]
    async fn the_four_kinds_with_no_store_behind_them_are_still_unstamped() {
        let relationship = relationship().await;
        relationship.engage(at(17)).await;
        let source = relationship.source();

        for kind in AudienceKind::ALL {
            if kind == AudienceKind::Engagement {
                continue;
            }
            let audience = roster_of(
                source
                    .living_audience(
                        PRINCIPAL,
                        WORKSPACE,
                        &AudienceRef::new(kind, relationship.counterparty_id.clone()),
                        at(11),
                    )
                    .await
                    .expect("the register is healthy"),
            );
            assert_eq!(
                audience.expires_at,
                None,
                "`{}` came back with an expiry, so the engagement branch is being applied to a \
                 kind no engagement governs",
                kind.as_str()
            );
        }
    }

    /// Phase 3's acceptance criterion, which nothing could meet until this
    /// surface existed: *the room discloses that access is logged, on the room
    /// itself* — not buried in a policy, and worded by the module that defines
    /// what is actually recorded, so the two cannot drift.
    #[actix_web::test]
    async fn the_room_discloses_that_access_is_logged() {
        let fixture = room(None);
        let index = fixture
            .surface()
            .open_room(&fixture.request(READER_SECRET), at(11))
            .await
            .expect("storage is healthy")
            .expect("the room is open");

        assert_eq!(index.notice, ROOM_LOGGING_NOTICE);
        assert!(index.notice.contains("logged"));
        assert!(
            index
                .notice
                .contains("records the link rather than the person"),
            "the notice must not claim the room knows who opened it"
        );
    }

    /// The secret must never reach a log line. A derived `Debug` on a request
    /// type that carries a live credential is the ordinary way that happens —
    /// one `{:?}` in an error path and the URL is in the journal.
    #[test]
    fn a_request_never_renders_its_secret() {
        let fixture = room(None);

        let room_request = format!("{:?}", fixture.request(READER_SECRET));
        assert!(!room_request.contains(READER_SECRET));
        assert!(room_request.contains("<redacted>"));

        let document_request = format!("{:?}", fixture.document(READER_SECRET, DECK));
        assert!(!document_request.contains(READER_SECRET));
        assert!(document_request.contains("<redacted>"));
    }

    /// Coarse, and deliberately not enough to fingerprint anyone. An absent or
    /// blank header is `Unknown` rather than a guess: a room is observable the
    /// way a place you visit is observable, and inventing a class from nothing
    /// is measurement nobody consented to.
    #[test]
    fn the_user_agent_class_is_coarse_and_absence_is_never_a_guess() {
        assert_eq!(classify_user_agent(None), UserAgentClass::Unknown);
        assert_eq!(classify_user_agent(Some("   ")), UserAgentClass::Unknown);
        assert_eq!(
            classify_user_agent(Some(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)"
            )),
            UserAgentClass::Mobile
        );
        assert_eq!(
            classify_user_agent(Some("Mozilla/5.0 (Linux; Android 15) Mobile Safari")),
            UserAgentClass::Mobile
        );
        assert_eq!(
            classify_user_agent(Some("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")),
            UserAgentClass::Desktop
        );
    }

    /// The HTTP layer's whole job: extract, and map. A served room must carry
    /// the notice, and the response must never carry the credential back — a
    /// body that echoed it would put a working capability into every cache and
    /// proxy on the way home.
    #[actix_web::test]
    async fn the_http_layer_serves_the_room_without_echoing_the_secret() {
        let fixture = room_at(Utc::now() - Duration::hours(1), None);
        let surface = web::Data::new(fixture.surface());
        let app = actix_test::init_service(
            App::new()
                .app_data(surface)
                .configure(configure_data_room_reader_routes),
        )
        .await;

        let request = actix_test::TestRequest::get()
            .uri(&format!(
                "/rooms/{}?workspace={WORKSPACE}&k={READER_SECRET}",
                fixture.room_id
            ))
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = actix_test::read_body(response).await;
        let rendered = String::from_utf8_lossy(&body);
        assert!(rendered.contains("Access to this room is logged"));
        assert!(rendered.contains(DECK));
        assert!(
            !rendered.contains(READER_SECRET),
            "the response must never carry the credential back: {rendered}"
        );
    }

    /// The refusal half of the same job: a withdrawn link must reach the
    /// reader as a status and a stable code, not as a `200` with an empty
    /// listing and not as a `500`.
    #[actix_web::test]
    async fn the_http_layer_maps_a_withdrawn_link_to_its_own_status_and_code() {
        let fixture = room_at(Utc::now() - Duration::hours(1), None);
        ShareLinkStore::new(fixture.workspace.clone())
            .revoke(
                &ShareLinkScope::new(PRINCIPAL, WORKSPACE),
                &fixture.room_id,
                READER,
                Utc::now(),
            )
            .expect("revoke the link");

        let surface = web::Data::new(fixture.surface());
        let app = actix_test::init_service(
            App::new()
                .app_data(surface)
                .configure(configure_data_room_reader_routes),
        )
        .await;

        let request = actix_test::TestRequest::get()
            .uri(&format!("/rooms/{}?workspace={WORKSPACE}", fixture.room_id))
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .insert_header((READER_KEY_HEADER, READER_SECRET))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "link_revoked");
        assert_eq!(
            body["message"],
            "This link has been withdrawn. Ask the sender for a new one."
        );
    }

    /// A request with no secret at all is refused on shape, without touching a
    /// store: the surface never treats "no credential presented" as a reason to
    /// go looking for one.
    #[actix_web::test]
    async fn a_request_carrying_no_secret_is_refused_on_shape() {
        let fixture = room_at(Utc::now() - Duration::hours(1), None);
        let surface = web::Data::new(fixture.surface());
        let app = actix_test::init_service(
            App::new()
                .app_data(surface)
                .configure(configure_data_room_reader_routes),
        )
        .await;

        let request = actix_test::TestRequest::get()
            .uri(&format!("/rooms/{}?workspace={WORKSPACE}", fixture.room_id))
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "malformed_request");
        assert!(fixture.events().is_empty());
    }
}
