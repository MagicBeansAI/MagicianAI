//! **The owner side of a shared room** — the grant path.
//!
//! Doc: `docs/components/magician/data-room.md`. Plan:
//! `docs/plans/2026-08-07-opc-deal-close.md`.
//!
//! `magician_api::data_room_reader_api` serves rooms that nothing could create,
//! over links nothing could issue: `DataRoomStore::open`, `for_audience`,
//! `add_document`, `withdraw_document`, `close` and every
//! `ShareLinkStore` grant method had no caller outside their own tests. This is
//! the surface that makes them reachable:
//!
//! - `POST   /data-rooms` — open a room for an audience;
//! - `GET    /data-rooms` — every room in the scope, or the one room a named
//!   audience has;
//! - `GET    /data-rooms/{room_id}` — one room and every grant ever made on it;
//! - `POST   /data-rooms/{room_id}/documents` — put a reference in the room;
//! - `DELETE /data-rooms/{room_id}/documents?ref=…` — take it out again;
//! - `POST   /data-rooms/{room_id}/links` — issue one identity's capability;
//! - `POST   /data-rooms/{room_id}/links/rotate` — replace that credential;
//! - `POST   /data-rooms/{room_id}/links/revoke` — kill it;
//! - `POST   /data-rooms/{room_id}/close` — close the room itself.
//!
//! It is mounted **inside** `/api/magician/v2`, unlike the reader. Everything
//! here is the owner acting on their own workspace, so it belongs behind the
//! same gate every other owner surface is behind — and the reader is outside
//! that gate precisely because a counterparty is not a workspace member.
//!
//! # A room binds an audience, never an engagement
//!
//! Every route that names a relationship takes an audience **kind** as a
//! parameter — `engagement`, `program`, `account`, `panel`, `person` — parsed
//! by [`AudienceKind::parse`], which refuses a word it does not know rather
//! than defaulting one. So a panel's audit pack, an account's deliverables and
//! a candidate's own offer are opened by the same route as a deal room, and a
//! second flow needs no edit here to exist. Nothing in this file names a deal,
//! a fundraise or a counterparty type.
//!
//! The roster itself is resolved through
//! [`AudienceSource`](crate::data_room_reader_api::AudienceSource) — the
//! reader's own trait, reused rather than copied. A second roster contract is
//! exactly the thing that would one day disagree with the one the reader
//! enforces, and then an owner would be told a room reaches people it does not.
//!
//! # Recording the disclosure fails the grant
//!
//! This is the rule the whole file is arranged around. Making a document
//! visible to somebody holding a live link **is** a disclosure, and the
//! outward-assertions register is where the workspace answers *who was told
//! what*. So:
//!
//! - adding a document records one act per `(document, live holder)` **before**
//!   the entry is appended — `DataRoomStore::add_document` owns that ordering
//!   and this surface hands it the live holder list;
//! - issuing or rotating a link records one act per `(present document,
//!   the identity being granted)` **before** the credential is minted, by
//!   calling [`record_room_disclosures`] directly.
//!
//! In both directions a failure to record leaves no grant at all. A grant that
//! succeeded while its record failed is a disclosure nobody can account for —
//! and no later sweep can tell that room from one nobody was ever shown. It
//! gets its own refusal code, `disclosure_not_recorded`, because *"we did not
//! grant it, because we could not account for it"* is the single most important
//! sentence this surface can say.
//!
//! # Removals never depend on the things grants depend on
//!
//! Revoking a link, withdrawing a document and closing a room resolve **no
//! roster and record no disclosure**. Fail-closed cuts the other way for a
//! removal: the end state the caller asked for is *less* access, and a
//! withdrawal that could be blocked by an unreadable register would leave a
//! credential alive because a *different* store was down. Grants refuse when
//! anything is unknown; removals proceed.
//!
//! # The secret exists for one response and is never stored
//!
//! `share_links` refuses to generate secrets — a module that both minted and
//! verified credentials would concentrate exactly what should stay split — so
//! entropy is this layer's job: 32 bytes of `OsRng`, hex, handed back once in
//! the issue or rotate response and never again. Only its blake3 hash reaches
//! disk. There is deliberately **no way for a caller to supply a secret**,
//! which is also what makes "a killed credential can never be re-armed"
//! unbreakable from outside: the dead-hash set refuses a revoked secret, and
//! nobody out here can name one in the first place.
//!
//! # Decisions and faults are separate returns
//!
//! Every method on [`OwnerSurface`] returns `Result<Result<T, Refusal>>`, the
//! same split the reader uses: the outer is storage, the inner is the decision.
//! A disk fault must never be reported as a policy refusal — telling an owner
//! their document was rejected when a log was unreadable is a claim about a
//! rule that never ran.
//!
//! Every refusal a caller can provoke is checked **before** the store is
//! called, so the store's own `anyhow` refusals are unreachable on the happy
//! path and any residual error is honestly a fault. Two callers racing can
//! still reach one — an issue and a rotate landing together — and that surfaces
//! as `503` rather than a wrong `409`. It under-reports the reason and never
//! over-grants.

use std::collections::BTreeSet;
use std::sync::Arc;

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{Audience, AudienceKind, AudienceRef};
use magician::magician_v2::evidence::{OutwardAssertionStore, OutwardScope};
use magician::magician_v2::share_links::{
    IssueShareLink, ShareLink, ShareLinkScope, ShareLinkStore,
};
use magician_learning::data_room::{
    names_a_revision, record_room_disclosures, DataRoom, DataRoomScope, DataRoomStore,
    DocumentVisibility, GrantDisclosure, OpenDataRoom, RoomStanding, REVISION_SEP,
};

use crate::approval_envelopes_api::guard_derivation_component;
use crate::data_room_reader_api::{is_derived_room_id, AudienceSource, AudienceStanding};
use crate::scope::resolve_required_scope;

/// How many bytes of entropy a capability secret carries.
///
/// The credential is the whole of a reader's authority, so it is sized as a
/// root secret rather than as a token somebody might type. Hex-encoded, it is
/// 64 characters in a URL.
const SECRET_BYTES: usize = 32;

// ── Scope ───────────────────────────────────────────────────────────────────

/// The tenant being acted on, resolved from the authenticated request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerScope {
    pub principal: String,
    pub workspace: String,
}

impl OwnerScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }

    fn rooms(&self) -> DataRoomScope {
        DataRoomScope::new(self.principal.as_str(), self.workspace.as_str())
    }

    fn links(&self) -> ShareLinkScope {
        ShareLinkScope::new(self.principal.as_str(), self.workspace.as_str())
    }

    fn outward(&self) -> OutwardScope {
        OutwardScope::new(self.principal.as_str(), self.workspace.as_str())
    }
}

// ── Refusals ────────────────────────────────────────────────────────────────

/// One way this surface says no, with the reason an owner can act on.
///
/// A struct rather than an enum because an owner refusal has to name the field
/// and the value that caused it — *"`visibility` must be `everyone` or
/// `identities`"* is actionable and *"invalid request"* is not. The `code` is
/// the stable machine-readable half and is what tests assert on; the message is
/// the human half and may be rewritten freely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub status: StatusCode,
    pub message: String,
}

impl Refusal {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            status,
            message: message.into(),
        }
    }

    fn malformed(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// The one refusal that is a fault rather than a decision.
    ///
    /// The register could not take the disclosure, so no grant was made. It is
    /// named rather than folded into the generic `503` because the whole
    /// ordering rule exists for this instant, and an owner retrying needs to
    /// know their counterparty was given nothing.
    fn unrecordable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "disclosure_not_recorded",
            "The disclosure could not be recorded, so no access was granted. Nothing has \
             changed for the recipient. Try again once the outward-assertions register is \
             writable.",
        )
    }

    fn response(&self) -> HttpResponse {
        HttpResponse::build(self.status).json(serde_json::json!({
            "error": self.code,
            "message": self.message,
        }))
    }
}

/// A caller-supplied string that will become part of a derived id, a file name
/// or a register key.
///
/// Delegates to the shared guard, which refuses blank, `U+001F` and any other
/// control character. `U+001F` is the separator every derived id in this set is
/// joined with: a value carrying one could shift bytes across it and fuse two
/// people's disclosures, or two documents', into a single record — after which
/// *"who saw what"* has a wrong answer rather than a missing one.
fn guard(label: &str, value: &str) -> Result<(), Refusal> {
    guard_derivation_component(label, value)
        .map_err(|message| Refusal::malformed("malformed_request", message))
}

/// Refuse an unpinned or ambiguous document reference, as a REQUEST fault.
///
/// The store refuses these too, and this is not a second rule: it asks
/// [`names_a_revision`] — the store's own predicate — so there is one definition
/// of what a pinned reference is and two places that enforce it.
///
/// It exists because of where the store's refusal LANDS. `add_document` returns
/// `anyhow::Error` for both a broken disk and a caller who forgot `@<revision>`,
/// the surface propagates it with `?`, and the handler maps that to
/// `unavailable` — a **503** whose message is *"Try again shortly."* Retrying an
/// unpinned reference fails identically forever, so the caller was told to do
/// the one thing that cannot work, while an operator watching 503s went looking
/// at a store that was perfectly healthy. Answering 400 here, with the remedy,
/// leaves the store's own bail as the backstop it should have been.
fn pinned_reference_refusal(artifact_ref: &str) -> Result<(), Refusal> {
    if names_a_revision(artifact_ref) {
        return Ok(());
    }
    let separators = artifact_ref.matches(REVISION_SEP).count();
    if separators > 1 {
        return Err(Refusal::malformed(
            "ambiguous_document_reference",
            format!(
                "`{artifact_ref}` carries {separators} `{REVISION_SEP}` separators, so which \
                 part names the revision is a guess. A later correction asking which rooms \
                 carry a claim would read the last one, match nothing, and silently fail to \
                 flag this room. Spell the artifact half without a separator"
            ),
        ));
    }
    Err(Refusal::malformed(
        "unpinned_document_reference",
        format!(
            "`{artifact_ref}` names no revision. Add it as \
             `{artifact_ref}{REVISION_SEP}<revision>`: an unpinned reference serves whatever \
             the artifact says at read time, so this room would show a different document \
             than the one that was cleared, and a later correction could not say whether it \
             carries the claim"
        ),
    ))
}

fn guard_scope(scope: &OwnerScope) -> Result<(), Refusal> {
    guard("the principal", &scope.principal)?;
    guard("the workspace", &scope.workspace)
}

fn guard_room_id(room_id: &str) -> Result<(), Refusal> {
    if is_derived_room_id(room_id) {
        return Ok(());
    }
    // Not merely fail-closed: `DataRoomStore` interpolates the id into a file
    // name under the authenticated scope, so an id shaped like a path is
    // refused before it ever reaches the store.
    Err(Refusal::malformed(
        "malformed_room_id",
        "a room id is `room-` followed by 32 hex digits; this store never minted anything else, \
         and the id becomes a file name under the authenticated scope",
    ))
}

// ── What a caller states ────────────────────────────────────────────────────

/// The relationship a room is for, as two words a caller can type.
///
/// The kind is a **parameter**, never an assumption. `AudienceKind::parse`
/// answers `None` for a word it does not know and this refuses on that answer,
/// because defaulting to `engagement` would file a panel's or an account's room
/// under the engagement key — and `AudienceRef::as_key` exists precisely so
/// those never merge.
fn parse_audience(kind: Option<&str>, id: Option<&str>) -> Result<AudienceRef, Refusal> {
    let kind = kind.map(str::trim).filter(|value| !value.is_empty());
    let id = id.map(str::trim).filter(|value| !value.is_empty());
    let (Some(kind), Some(id)) = (kind, id) else {
        return Err(Refusal::malformed(
            "audience_required",
            format!(
                "both `audience_kind` ({}) and `audience_id` are required; a room is the \
                 document channel OF a relationship, and without one there is nothing for \
                 access to derive from",
                kind_words()
            ),
        ));
    };
    let Some(kind) = AudienceKind::parse(kind) else {
        return Err(Refusal::malformed(
            "unknown_audience_kind",
            format!(
                "`{kind}` is not an audience kind; state one of {}. There is no default: \
                 guessing would open a room under a relationship the caller did not name",
                kind_words()
            ),
        ));
    };
    guard("an audience id", id)?;
    Ok(AudienceRef::new(kind, id))
}

fn kind_words() -> String {
    AudienceKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Who, among the audience, a document is for.
///
/// **There is no default.** An absent `visibility` refuses rather than reading
/// as `everyone`: a widening nobody typed is the one mistake this field can
/// make, and it would be invisible in the request that made it.
fn parse_visibility(
    visibility: Option<&str>,
    identities: Option<&Vec<String>>,
) -> Result<DocumentVisibility, Refusal> {
    let Some(word) = visibility.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(Refusal::malformed(
            "visibility_required",
            "state `visibility` as `everyone` (every identity in the audience) or `identities` \
             (only those named); there is no default, because a default would widen a grant \
             nobody asked for",
        ));
    };
    match word.to_ascii_lowercase().as_str() {
        "everyone" => {
            // Naming identities under `everyone` reads like a restriction and
            // is not one. Refusing is the difference between an owner who
            // learns their request was contradictory and an owner who believes
            // a document is restricted when the whole audience can open it.
            if identities.is_some_and(|named| !named.is_empty()) {
                return Err(Refusal::malformed(
                    "visibility_conflict",
                    "`everyone` means every identity in the audience, so naming identities \
                     alongside it would read as a restriction that is not one; state \
                     `identities` to restrict",
                ));
            }
            Ok(DocumentVisibility::Everyone)
        },
        "identities" => {
            let named: &[String] = identities.map(|held| held.as_slice()).unwrap_or_default();
            if named.is_empty() {
                return Err(Refusal::malformed(
                    "identities_required",
                    "`identities` restricts a document to the people named, so the list must \
                     name somebody; an empty one is a document nobody may open, which is a \
                     caller mistake rather than a grant",
                ));
            }
            for identity in named {
                guard("a named identity", identity)?;
            }
            Ok(DocumentVisibility::Identities {
                identities: normalise_identities(named),
            })
        },
        other => Err(Refusal::malformed(
            "unknown_visibility",
            format!(
                "`{other}` is not a visibility; state `everyone` or `identities`. There is no \
                 default and no near match: a guess here decides who can read a document"
            ),
        )),
    }
}

/// Trimmed, deduplicated and ordered, so the same restriction typed twice is
/// one restriction.
///
/// Without it, `[a, b]` and `[b, a, b]` would compare unequal and a replayed
/// add would look like a changed payload — which this surface refuses. The
/// order is not a preference; it is what makes the replay check honest.
fn normalise_identities(identities: &[String]) -> Vec<String> {
    identities
        .iter()
        .map(|identity| identity.trim().to_string())
        .filter(|identity| !identity.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether two visibilities say the same thing, whatever order they were typed
/// in.
fn same_visibility(held: &DocumentVisibility, wanted: &DocumentVisibility) -> bool {
    match (held, wanted) {
        (DocumentVisibility::Everyone, DocumentVisibility::Everyone) => true,
        (
            DocumentVisibility::Identities { identities: held },
            DocumentVisibility::Identities { identities: wanted },
        ) => normalise_identities(held) == normalise_identities(wanted),
        _ => false,
    }
}

// ── What a caller is told ───────────────────────────────────────────────────

/// One document in a room, as its owner sees it.
///
/// Withdrawn entries are **included and marked**, never dropped: *"this was in
/// the room between March and April"* is the question an audit asks, and a
/// listing that hid it could not answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentView {
    pub reference: String,
    /// `everyone` or `identities`.
    pub visibility: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<String>,
    pub added_at: DateTime<Utc>,
    pub added_by: String,
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withdrawn_at: Option<DateTime<Utc>>,
}

/// One grant on a room. **Never the secret, and never its hash.**
///
/// The plaintext exists for exactly one response. The hash is withheld too: it
/// is a stored credential fingerprint, and nothing an owner does with this view
/// needs it. `rotated` reports that this credential replaced another without
/// naming the hash it replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkView {
    pub link_id: String,
    pub issued_to: String,
    pub audience_kind: &'static str,
    pub audience_id: String,
    /// Derived from the clock at read time, never stored: `live`, `expired` or
    /// `revoked`. A link must read as dead even if nothing has run since.
    pub state: &'static str,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    pub rotated: bool,
    /// How many times this grant slot's credential has been presented. A count,
    /// never a rate, and not the number of times the room was opened — the
    /// access lane owns that.
    pub presentations: u32,
}

impl LinkView {
    fn of(link: &ShareLink, now: DateTime<Utc>) -> Self {
        Self {
            link_id: link.link_id.clone(),
            issued_to: link.issued_to.clone(),
            audience_kind: link.audience.kind.as_str(),
            audience_id: link.audience.id.clone(),
            state: link.state(now).as_str(),
            issued_at: link.issued_at,
            expires_at: link.expires_at,
            revoked_at: link.revoked_at,
            rotated: link.rotation_of.is_some(),
            presentations: link.presentations,
        }
    }
}

/// A room, as its owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoomView {
    pub room_id: String,
    pub audience_kind: &'static str,
    pub audience_id: String,
    /// `open`, `expired` or `closed`, derived from the clock at read time. A
    /// stored status would drift the moment a room expired with nobody writing
    /// to it.
    pub standing: &'static str,
    pub opened_at: DateTime<Utc>,
    pub opened_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closes_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    pub documents: Vec<DocumentView>,
    /// Counts, never rates.
    pub documents_present: usize,
    pub live_links: usize,
    pub links_ever: usize,
}

impl RoomView {
    fn of(room: &DataRoom, links: &[ShareLink], now: DateTime<Utc>) -> Self {
        Self {
            room_id: room.room_id.clone(),
            audience_kind: room.audience.kind.as_str(),
            audience_id: room.audience.id.clone(),
            standing: room.standing(now).as_str(),
            opened_at: room.opened_at,
            opened_by: room.opened_by.clone(),
            closes_at: room.closes_at,
            closed_at: room.closed_at,
            documents: room
                .documents
                .iter()
                .map(|entry| DocumentView {
                    reference: entry.artifact_ref.clone(),
                    visibility: match entry.visibility {
                        DocumentVisibility::Everyone => "everyone",
                        DocumentVisibility::Identities { .. } => "identities",
                    },
                    identities: match &entry.visibility {
                        DocumentVisibility::Everyone => Vec::new(),
                        DocumentVisibility::Identities { identities } => identities.clone(),
                    },
                    added_at: entry.added_at,
                    added_by: entry.added_by.clone(),
                    present: entry.is_present(),
                    withdrawn_at: entry.withdrawn_at,
                })
                .collect(),
            documents_present: room.present_documents().len(),
            live_links: links.iter().filter(|link| link.is_live(now)).count(),
            links_ever: links.len(),
        }
    }
}

/// One room and every grant ever made on it, dead ones included: *who could
/// ever have opened this* is a question about the whole history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoomDetail {
    pub room: RoomView,
    pub links: Vec<LinkView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoomListing {
    pub rooms: Vec<RoomView>,
    pub count: usize,
}

/// A minted credential, returned **once**.
///
/// No `Debug`: the secret lives in here, and a derived one is how a working
/// credential ends up in a log line.
#[derive(Clone, Serialize)]
pub struct IssuedLink {
    pub link: LinkView,
    /// The plaintext capability. Shown here and never again — only its blake3
    /// hash reaches disk. Put it in the reader's `X-Room-Key` header, or the
    /// `k` query parameter when it has to survive being a URL.
    pub secret: String,
    /// How many `(document, holder)` disclosures this grant put in the
    /// outward-assertions register. A count, never a rate. Zero is honest: an
    /// empty room discloses nothing.
    pub disclosures_recorded: usize,
}

impl std::fmt::Debug for IssuedLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedLink")
            .field("link", &self.link)
            .field("secret", &"<redacted>")
            .field("disclosures_recorded", &self.disclosures_recorded)
            .finish()
    }
}

/// What a revocation did, and what the identity holds afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevokedLinks {
    pub links: Vec<LinkView>,
    /// How many credentials this call killed. Zero means they held nothing
    /// unrevoked — a no-op success, because the end state asked for already
    /// held.
    pub revoked_now: usize,
}

// ── What a caller supplies ──────────────────────────────────────────────────

/// Open a room. `closes_at` is the room's **own** clock and is optional; the
/// relationship's clock always applies on top of it.
#[derive(Debug, Clone)]
pub struct OpenRoomRequest {
    pub audience: AudienceRef,
    pub opened_by: String,
    pub closes_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct AddDocumentRequest {
    pub room_id: String,
    pub artifact_ref: String,
    pub visibility: DocumentVisibility,
    /// Who put it in the room. Also the disclosure's `effective_sender`: one
    /// act, one actor, and no second field that could disagree with this one.
    pub added_by: String,
}

/// Issue or rotate a capability.
///
/// `expires_at` is an `Option` **only so that its absence can be refused with a
/// reason**. A possession-based grant that never lapses is the blanket-yes
/// failure — the URL outlives the relationship that justified it — so there is
/// no default to fall back to, and a request without one is turned away rather
/// than given a deadline nobody chose.
#[derive(Debug, Clone)]
pub struct GrantLinkRequest {
    pub room_id: String,
    pub issued_to: String,
    pub expires_at: Option<DateTime<Utc>>,
    /// Who granted it — the register's answer to *who told them*.
    pub issued_by: String,
}

// ── The surface ─────────────────────────────────────────────────────────────

/// The decision layer. The HTTP handlers below do extraction and status mapping
/// and nothing else, so every refusal in this file is testable without a
/// server.
pub struct OwnerSurface {
    rooms: DataRoomStore,
    links: ShareLinkStore,
    assertions: OutwardAssertionStore,
    audiences: Arc<dyn AudienceSource>,
}

impl OwnerSurface {
    pub fn new(workspace: ArtifactV2Workspace, audiences: Arc<dyn AudienceSource>) -> Self {
        Self {
            rooms: DataRoomStore::new(workspace.clone()),
            links: ShareLinkStore::new(workspace.clone()),
            assertions: OutwardAssertionStore::new(workspace),
            audiences,
        }
    }

    /// Open a room for an audience, or resume the one that is already open.
    ///
    /// # One audience, one room — and a changed payload is not a replay
    ///
    /// `DataRoomStore::open` returns the existing room when one exists, which
    /// makes a retried call resume. It cannot tell a retry from a caller asking
    /// for **different** terms, though, and silently returning the old room
    /// would let an owner believe they had moved a closing date that never
    /// moved. So this compares the two fields a caller states — `opened_by` and
    /// `closes_at` — and refuses when they differ.
    ///
    /// # A closed room does not re-open
    ///
    /// Closing is terminal, and the store has no un-close. Asking to open a
    /// closed room says so rather than handing back a closed room to somebody
    /// who asked for an open one.
    pub async fn open_room(
        &self,
        scope: &OwnerScope,
        request: &OpenRoomRequest,
        now: DateTime<Utc>,
    ) -> Result<Result<RoomView, Refusal>> {
        if let Err(refusal) = guard_scope(scope) {
            return Ok(Err(refusal));
        }
        if let Err(refusal) = guard("an audience id", &request.audience.id) {
            return Ok(Err(refusal));
        }
        if let Err(refusal) = guard("`opened_by`", &request.opened_by) {
            return Ok(Err(refusal));
        }
        // Expiry is inclusive everywhere in this codebase, so a closing date
        // equal to `now` is a room born closed. Refusing surfaces a caller bug
        // that would otherwise look like a room nobody could read.
        if request.closes_at.is_some_and(|closes| now >= closes) {
            return Ok(Err(Refusal::malformed(
                "closing_date_not_ahead_of_now",
                "a room's closing date must be ahead of now — expiry is inclusive, so a room \
                 closing at this instant is already closed and could never be read",
            )));
        }

        // Resolved and then deliberately unused: opening a room discloses
        // nothing, because an empty room has nothing to disclose and no link
        // yet exists to disclose it to. It is resolved so that a room is never
        // opened against a relationship that is unknown, unreadable or already
        // over — each of which would produce a room nobody could ever read.
        if let Err(refusal) = self.living(scope, &request.audience, now).await? {
            return Ok(Err(refusal));
        }
        let room_scope = scope.rooms();

        if let Some(existing) = self.rooms.for_audience(&room_scope, &request.audience)? {
            if existing.closed_at.is_some() {
                return Ok(Err(Refusal::conflict(
                    "room_closed",
                    format!(
                        "the room for `{}` was closed and closing is terminal; a closed room \
                         never re-opens, because the record of what was shared while it was \
                         open has to keep meaning what it says",
                        request.audience.as_key()
                    ),
                )));
            }
            if existing.opened_by != request.opened_by || existing.closes_at != request.closes_at {
                return Ok(Err(Refusal::conflict(
                    "room_already_open_on_other_terms",
                    format!(
                        "a room is already open for `{}` on different terms; an identical \
                         request resumes it, but a changed one is an error rather than a \
                         silent no-op — the store would have returned the existing room and \
                         ignored the change",
                        request.audience.as_key()
                    ),
                )));
            }
            let links = self.links.for_resource(&scope.links(), &existing.room_id)?;
            return Ok(Ok(RoomView::of(&existing, &links, now)));
        }

        let room = self.rooms.open(
            &room_scope,
            &OpenDataRoom {
                audience: request.audience.clone(),
                opened_by: request.opened_by.clone(),
                closes_at: request.closes_at,
            },
            now,
        )?;
        Ok(Ok(RoomView::of(&room, &[], now)))
    }

    /// Every room in the scope, or the one room a named audience has.
    ///
    /// A half-named audience — a kind with no id, or an id with no kind —
    /// refuses rather than falling back to the full listing. Falling back would
    /// answer a narrow question with a wide answer that looks correct.
    pub fn list_rooms(
        &self,
        scope: &OwnerScope,
        audience: Option<&AudienceRef>,
        now: DateTime<Utc>,
    ) -> Result<Result<RoomListing, Refusal>> {
        if let Err(refusal) = guard_scope(scope) {
            return Ok(Err(refusal));
        }
        let room_scope = scope.rooms();
        let rooms: Vec<DataRoom> = match audience {
            Some(reference) => self
                .rooms
                .for_audience(&room_scope, reference)?
                .into_iter()
                .collect(),
            None => self.rooms.list(&room_scope)?,
        };

        let link_scope = scope.links();
        let mut views = Vec::with_capacity(rooms.len());
        for room in &rooms {
            let links = self.links.for_resource(&link_scope, &room.room_id)?;
            views.push(RoomView::of(room, &links, now));
        }
        Ok(Ok(RoomListing {
            count: views.len(),
            rooms: views,
        }))
    }

    /// One room and every grant ever made on it.
    pub fn room(
        &self,
        scope: &OwnerScope,
        room_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<RoomDetail, Refusal>> {
        let (room, links) = match self.room_and_links(scope, room_id)? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        Ok(Ok(RoomDetail {
            room: RoomView::of(&room, &links, now),
            links: links.iter().map(|link| LinkView::of(link, now)).collect(),
        }))
    }

    /// Put a reference in the room — the visibility grant.
    ///
    /// The live holder list is read here and handed to
    /// `DataRoomStore::add_document`, which records one disclosure per
    /// `(document, admitted holder)` **before** the entry is appended. If that
    /// recording fails the append never runs, so a document is never visible
    /// and unaccounted for. An empty holder list is an honest absence — no
    /// link, no grant to account for — and never a licence: every act that is
    /// recorded names exactly one holder.
    ///
    /// # A re-add with different terms is an error
    ///
    /// The store is idempotent on `artifact_ref` and returns the room unchanged
    /// when the document is already present — including when the caller asked
    /// for a **different** visibility, which would silently ignore a
    /// restriction somebody meant to apply. This refuses that case and names
    /// the remedy: withdraw the document, then add it again.
    pub async fn add_document(
        &self,
        scope: &OwnerScope,
        request: &AddDocumentRequest,
        now: DateTime<Utc>,
    ) -> Result<Result<RoomView, Refusal>> {
        if let Err(refusal) = guard("an artifact reference", &request.artifact_ref) {
            return Ok(Err(refusal));
        }
        if let Err(refusal) = pinned_reference_refusal(&request.artifact_ref) {
            return Ok(Err(refusal));
        }
        if let Err(refusal) = guard("`added_by`", &request.added_by) {
            return Ok(Err(refusal));
        }
        let (room, links) = match self.room_and_links(scope, &request.room_id)? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if let Some(refusal) = closed_room_refusal(&room, now) {
            return Ok(Err(refusal));
        }
        let audience = match self.living(scope, &room.audience, now).await? {
            Ok(audience) => audience,
            Err(refusal) => return Ok(Err(refusal)),
        };

        if let Some(held) = room
            .documents
            .iter()
            .find(|entry| entry.artifact_ref == request.artifact_ref && entry.is_present())
        {
            if !same_visibility(&held.visibility, &request.visibility) {
                return Ok(Err(Refusal::conflict(
                    "document_visibility_conflict",
                    format!(
                        "`{}` is already in this room under a different visibility, and adding \
                         it again would be ignored rather than applied; withdraw it first, then \
                         add it with the visibility you want",
                        request.artifact_ref
                    ),
                )));
            }
        }

        // Everyone holding a live credential on this room right now. Read from
        // the grant log rather than tracked anywhere, so a link issued a minute
        // ago is a holder and one revoked a minute ago is not.
        let holders = live_holders(&links, now);
        let grant = GrantDisclosure {
            assertions: &self.assertions,
            audience: &audience,
            holders: &holders,
            disclosed_by: &request.added_by,
        };
        let room = self.rooms.add_document(
            &scope.rooms(),
            &room.room_id,
            &request.artifact_ref,
            request.visibility.clone(),
            &request.added_by,
            &grant,
            now,
        )?;
        let links = self.links.for_resource(&scope.links(), &room.room_id)?;
        Ok(Ok(RoomView::of(&room, &links, now)))
    }

    /// Take a reference out of the room.
    ///
    /// A removal: it resolves no roster and records no disclosure. Nothing new
    /// is being told to anybody, and a withdrawal that could be blocked by an
    /// unreadable register would leave a document visible because a *different*
    /// store was down.
    ///
    /// The entry is kept and marked rather than deleted, and withdrawing a
    /// document that is not there is a no-op success — the end state asked for
    /// already holds.
    pub fn withdraw_document(
        &self,
        scope: &OwnerScope,
        room_id: &str,
        artifact_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<RoomView, Refusal>> {
        if let Err(refusal) = guard("an artifact reference", artifact_ref) {
            return Ok(Err(refusal));
        }
        let (room, _) = match self.room_and_links(scope, room_id)? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let room =
            self.rooms
                .withdraw_document(&scope.rooms(), &room.room_id, artifact_ref, now)?;
        let links = self.links.for_resource(&scope.links(), &room.room_id)?;
        Ok(Ok(RoomView::of(&room, &links, now)))
    }

    /// Close the room.
    ///
    /// A removal, like a withdrawal: no roster, no disclosure. Idempotent, and
    /// every document entry is kept — what was in the room is exactly what an
    /// owner needs after closing it.
    pub fn close_room(
        &self,
        scope: &OwnerScope,
        room_id: &str,
        closed_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<RoomView, Refusal>> {
        if let Err(refusal) = guard("`closed_by`", closed_by) {
            return Ok(Err(refusal));
        }
        let (room, _) = match self.room_and_links(scope, room_id)? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let room = self
            .rooms
            .close(&scope.rooms(), &room.room_id, closed_by, now)?;
        let links = self.links.for_resource(&scope.links(), &room.room_id)?;
        Ok(Ok(RoomView::of(&room, &links, now)))
    }

    /// Issue one identity's capability into a room.
    ///
    /// One identity, one link: the store refuses to stack a second live
    /// credential, and this checks first so the caller is told to rotate rather
    /// than handed a fault.
    ///
    /// The disclosure is recorded **before** the credential is minted. Every
    /// document already in the room becomes visible to this identity the
    /// instant the link exists, and that is a disclosure like any other.
    pub async fn issue_link(
        &self,
        scope: &OwnerScope,
        request: &GrantLinkRequest,
        now: DateTime<Utc>,
    ) -> Result<Result<IssuedLink, Refusal>> {
        let prepared = match self.prepare_grant(scope, request, now).await? {
            Ok(prepared) => prepared,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if self
            .links
            .live_link(
                &scope.links(),
                &prepared.room.room_id,
                &request.issued_to,
                now,
            )?
            .is_some()
        {
            return Ok(Err(Refusal::conflict(
                "link_already_live",
                format!(
                    "`{}` already holds a live link to this room; one identity holds one link so \
                     revocation has exactly one target — rotate the existing credential instead \
                     of stacking a second",
                    request.issued_to
                ),
            )));
        }
        self.grant(scope, &prepared, request, false, now)
    }

    /// Replace an identity's live credential: the old one dies, a fresh secret
    /// takes its place in the same grant slot, and the presentation count
    /// carries over because it belongs to the identity rather than to the
    /// secret in their hands.
    ///
    /// There is deliberately no "extend": a longer life means a fresh secret,
    /// so a leaked URL can never be silently made longer-lived.
    ///
    /// Like an issue, the disclosure is recorded before the credential is
    /// minted, and a recording failure leaves the rotation undone. The old
    /// credential therefore stays live — which is why
    /// [`revoke_link`](Self::revoke_link) records nothing and can always run:
    /// killing a leaked URL must never depend on the register.
    pub async fn rotate_link(
        &self,
        scope: &OwnerScope,
        request: &GrantLinkRequest,
        now: DateTime<Utc>,
    ) -> Result<Result<IssuedLink, Refusal>> {
        let prepared = match self.prepare_grant(scope, request, now).await? {
            Ok(prepared) => prepared,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if self
            .links
            .live_link(
                &scope.links(),
                &prepared.room.room_id,
                &request.issued_to,
                now,
            )?
            .is_none()
        {
            return Ok(Err(Refusal::conflict(
                "no_live_link",
                format!(
                    "`{}` holds no live link to this room, so there is nothing to rotate; a \
                     caller who believes an old credential was just invalidated has to find out \
                     none existed — issue one instead",
                    request.issued_to
                ),
            )));
        }
        self.grant(scope, &prepared, request, true, now)
    }

    /// Kill every unrevoked credential an identity holds on a room.
    ///
    /// **Forward-only**: it prevents future presentation and recalls nothing
    /// already fetched, and the presentations accumulated before the kill stay
    /// on the record — erasing them would falsify the audit log.
    ///
    /// **Resolves no roster and records no disclosure.** This is the act an
    /// owner reaches for when a URL has gone somewhere it should not have, and
    /// it must not be blocked because a register or a counterparty store is
    /// unreadable. A revoked credential can never come back either: the store's
    /// dead-hash set refuses its secret for good, and no caller of this surface
    /// can name a secret in the first place.
    pub fn revoke_link(
        &self,
        scope: &OwnerScope,
        room_id: &str,
        issued_to: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<RevokedLinks, Refusal>> {
        if let Err(refusal) = guard("`issued_to`", issued_to) {
            return Ok(Err(refusal));
        }
        let (room, _) = match self.room_and_links(scope, room_id)? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let links = self
            .links
            .revoke(&scope.links(), &room.room_id, issued_to, now)?;
        Ok(Ok(RevokedLinks {
            revoked_now: links
                .iter()
                .filter(|link| link.revoked_at == Some(now))
                .count(),
            links: links.iter().map(|link| LinkView::of(link, now)).collect(),
        }))
    }

    // ── Internals ───────────────────────────────────────────────────────────

    /// The room, its grants, and the refusals that stop either from being read.
    fn room_and_links(
        &self,
        scope: &OwnerScope,
        room_id: &str,
    ) -> Result<Result<(DataRoom, Vec<ShareLink>), Refusal>> {
        if let Err(refusal) = guard_scope(scope) {
            return Ok(Err(refusal));
        }
        if let Err(refusal) = guard_room_id(room_id) {
            return Ok(Err(refusal));
        }
        let Some(room) = self.rooms.load(&scope.rooms(), room_id)? else {
            return Ok(Err(Refusal::not_found(
                "room_not_found",
                format!("no data room `{room_id}` in this workspace"),
            )));
        };
        let links = self.links.for_resource(&scope.links(), &room.room_id)?;
        Ok(Ok((room, links)))
    }

    /// The living roster of a relationship, or the reason there is not one.
    ///
    /// [`AudienceStanding::Unknown`] means **no such counterparty**, and
    /// [`AudienceStanding::NeverEngaged`] means we know exactly who they are
    /// and have never been in a relationship with them. Both refuse. `Err`
    /// means the source could not be read, which propagates as a fault. An
    /// unknown, a never-established and an unreadable roster are none of them
    /// permission to grant anything.
    ///
    /// The never-established case is refused **on its own code** rather than as
    /// `relationship_ended`. An owner told a relationship ended reaches for
    /// revoke, withdraw or close, and not one of those three can bring into
    /// being a relationship nobody ever recorded — so the message that named
    /// them was pointing at the only acts guaranteed not to help.
    ///
    /// A roster answering for a *different* relationship — the shape a merged
    /// away organisation produces, where the surviving record answers for the
    /// folded one — refuses rather than being accepted: a room pointed at a
    /// folded-away id has to be re-pointed by its owner, never silently
    /// re-aimed.
    ///
    /// A relationship that has ended refuses too, on every path that **grants**.
    /// Access derives from the relationship and ends with it, so a document
    /// added or a link issued into a dead relationship could never be read —
    /// and a room that looked shared but was not is worse than a refusal.
    async fn living(
        &self,
        scope: &OwnerScope,
        reference: &AudienceRef,
        now: DateTime<Utc>,
    ) -> Result<Result<Audience, Refusal>> {
        let audience = match self
            .audiences
            .living_audience(&scope.principal, &scope.workspace, reference, now)
            .await?
        {
            AudienceStanding::Roster(audience) => audience,
            AudienceStanding::NeverEngaged => {
                return Ok(Err(Refusal::conflict(
                    "relationship_not_started",
                    format!(
                        "nothing has ever put this workspace in a relationship `{}` — no \
                         engagement with them is on file at all, past or present — so there \
                         is nothing for this room's access to derive from. Record the \
                         engagement first: this relationship has not started, it has not \
                         finished, and there is no grant here to undo",
                        reference.as_key()
                    ),
                )))
            },
            AudienceStanding::Unknown => {
                return Ok(Err(Refusal::not_found(
                    "relationship_unknown",
                    format!(
                        "no relationship `{}` could be established, and an unknown \
                         relationship is never permission to share anything",
                        reference.as_key()
                    ),
                )))
            },
        };
        if audience.reference != *reference {
            return Ok(Err(Refusal::conflict(
                "audience_mismatch",
                format!(
                    "the roster that answered for `{}` is `{}` — the shape a merged-away record \
                     produces. Re-point this at the relationship that survived rather than \
                     sharing under a name that no longer means what it did",
                    reference.as_key(),
                    audience.reference.as_key()
                ),
            )));
        }
        if !audience.is_current(now) {
            return Ok(Err(Refusal::conflict(
                "relationship_ended",
                format!(
                    "the relationship `{}` has ended, and access derives from it — anything \
                     granted now could never be read. Removals still work: revoke, withdraw and \
                     close do not ask about the relationship",
                    reference.as_key()
                ),
            )));
        }
        Ok(Ok(audience))
    }

    /// Everything an issue and a rotate check identically, before either
    /// touches the grant log.
    async fn prepare_grant(
        &self,
        scope: &OwnerScope,
        request: &GrantLinkRequest,
        now: DateTime<Utc>,
    ) -> Result<Result<PreparedGrant, Refusal>> {
        if let Err(refusal) = guard("`issued_to`", &request.issued_to) {
            return Ok(Err(refusal));
        }
        if let Err(refusal) = guard("`issued_by`", &request.issued_by) {
            return Ok(Err(refusal));
        }
        // The mandatory expiry, refused rather than defaulted. A grant whose
        // deadline this surface chose would be a deadline nobody agreed to.
        let Some(expires_at) = request.expires_at else {
            return Ok(Err(Refusal::malformed(
                "expiry_required",
                "a share link must state `expires_at`; a possession-based grant that never \
                 lapses is a standing yes to whoever holds the URL, and there is no default \
                 this surface may pick on a caller's behalf",
            )));
        };
        if now >= expires_at {
            return Ok(Err(Refusal::malformed(
                "expiry_not_ahead_of_now",
                "a share link must expire ahead of issuance, and expiry is inclusive — a grant \
                 born lapsed hides a caller bug behind a credential that opens nothing",
            )));
        }

        let (room, _) = match self.room_and_links(scope, &request.room_id)? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if let Some(refusal) = closed_room_refusal(&room, now) {
            return Ok(Err(refusal));
        }
        let audience = match self.living(scope, &room.audience, now).await? {
            Ok(audience) => audience,
            Err(refusal) => return Ok(Err(refusal)),
        };
        // `admits` is an `any` over the roster, so an audience naming nobody
        // admits nobody: the empty roster fails closed rather than vacuously
        // passing. A credential issued to somebody the roster does not name
        // opens nothing today and would spring to life the day they were added,
        // without anybody deciding that — so it is refused now.
        if !audience.admits(&request.issued_to, now) {
            return Ok(Err(Refusal::forbidden(
                "identity_not_admitted",
                format!(
                    "`{}` is not in the audience `{}`, so a credential issued to them would \
                     open nothing today and would come alive the day somebody added them — \
                     without anyone deciding to grant it",
                    request.issued_to,
                    room.audience.as_key()
                ),
            )));
        }
        Ok(Ok(PreparedGrant {
            room,
            audience,
            expires_at,
        }))
    }

    /// Record, then mint. The order is the whole point.
    ///
    /// If [`record_room_disclosures`] fails, `issue`/`rotate` never runs and
    /// there is no credential at all. The reverse order cannot give that: a
    /// grant that succeeded while its record failed is a disclosure nobody can
    /// account for, and no later sweep can distinguish that room from one
    /// nobody was ever shown.
    ///
    /// Recording first also means a mint that then fails leaves acts describing
    /// a grant that did not happen. That is the safe side of the trade and it
    /// self-heals: the acts are keyed on `(room, document, holder)`, so a retry
    /// resumes the same records rather than opening second ones.
    fn grant(
        &self,
        scope: &OwnerScope,
        prepared: &PreparedGrant,
        request: &GrantLinkRequest,
        rotating: bool,
        now: DateTime<Utc>,
    ) -> Result<Result<IssuedLink, Refusal>> {
        let holders = vec![request.issued_to.clone()];
        let recorded = match record_room_disclosures(
            &self.assertions,
            &scope.outward(),
            &prepared.room,
            &prepared.audience,
            &holders,
            &request.issued_by,
            now,
            &now.to_rfc3339(),
        ) {
            Ok(recorded) => recorded,
            Err(error) => {
                tracing::error!(
                    error = ?error,
                    room_id = %prepared.room.room_id,
                    "refusing a data room grant: its disclosure could not be recorded"
                );
                return Ok(Err(Refusal::unrecordable()));
            },
        };

        let issue = IssueShareLink {
            resource_ref: prepared.room.room_id.clone(),
            audience: prepared.room.audience.clone(),
            issued_to: request.issued_to.clone(),
            secret: mint_secret(),
            expires_at: prepared.expires_at,
        };
        let link_scope = scope.links();
        let link = if rotating {
            self.links.rotate(&link_scope, &issue, now)?
        } else {
            self.links.issue(&link_scope, &issue, now)?
        };
        Ok(Ok(IssuedLink {
            link: LinkView::of(&link, now),
            secret: issue.secret,
            disclosures_recorded: recorded.len(),
        }))
    }
}

/// What both grant paths establish before either writes.
struct PreparedGrant {
    room: DataRoom,
    audience: Audience,
    expires_at: DateTime<Utc>,
}

/// A room's own clock and the owner's own verdict, as a refusal.
///
/// A closed or expired room takes no document and issues no credential: adding
/// to one would make it look, later, as though a document was available when it
/// was not, and a credential into one opens nothing.
fn closed_room_refusal(room: &DataRoom, now: DateTime<Utc>) -> Option<Refusal> {
    let standing = room.standing(now);
    let code = match standing {
        RoomStanding::Open => return None,
        RoomStanding::Closed => "room_closed",
        RoomStanding::Expired => "room_expired",
    };
    Some(Refusal::conflict(
        code,
        format!(
            "this room is {} and grants nothing further; a document added to it would read, \
             later, as though it had been available when it was not",
            standing.as_str()
        ),
    ))
}

/// Everyone holding an unrevoked, unexpired credential on a room right now.
///
/// Deduplicated and ordered so the same holder cannot be counted twice: the
/// disclosure bridge collapses duplicates anyway, but a doubled holder would
/// also double the count reported back to the owner.
fn live_holders(links: &[ShareLink], now: DateTime<Utc>) -> Vec<String> {
    links
        .iter()
        .filter(|link| link.is_live(now))
        .map(|link| link.issued_to.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A fresh capability secret.
///
/// `share_links` refuses to generate one on purpose — a module that both minted
/// and verified credentials would concentrate what should stay split across
/// layers — so entropy is this layer's job. `OsRng`, never a thread RNG seeded
/// from anything guessable: this is the entire authority of whoever holds it.
fn mint_secret() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// ── HTTP ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RoomScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RoomListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DocumentRefQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// The document, as a query parameter rather than a path segment: an
    /// artifact reference may carry slashes and `@revision`, and a path segment
    /// would force every caller to escape it identically or withdraw the wrong
    /// thing.
    #[serde(rename = "ref", default)]
    pub reference: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenRoomBody {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
    #[serde(default)]
    pub opened_by: Option<String>,
    #[serde(default)]
    pub closes_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct AddDocumentBody {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub artifact_ref: Option<String>,
    #[serde(default)]
    pub added_by: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub identities: Option<Vec<String>>,
}

/// Issue or rotate. **No secret field, deliberately**: a caller cannot supply a
/// credential, so a killed one can never be named back into existence.
#[derive(Debug, Deserialize)]
pub struct GrantLinkBody {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub issued_to: Option<String>,
    #[serde(default)]
    pub issued_by: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct RevokeLinkBody {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub issued_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CloseRoomBody {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub closed_by: Option<String>,
}

/// `POST /data-rooms` — open a room for an audience.
pub async fn open_data_room_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    body: web::Json<OpenRoomBody>,
) -> HttpResponse {
    let scope = match owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match parse_audience(body.audience_kind.as_deref(), body.audience_id.as_deref())
    {
        Ok(audience) => audience,
        Err(refusal) => return refusal.response(),
    };
    let request = OpenRoomRequest {
        audience,
        opened_by: body.opened_by.clone().unwrap_or_default(),
        closes_at: body.closes_at,
    };
    match surface.open_room(&scope, &request, Utc::now()).await {
        Ok(Ok(room)) => HttpResponse::Ok().json(room),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

/// `GET /data-rooms` — every room, or the one a named audience has.
pub async fn list_data_rooms_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    query: web::Query<RoomListQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    // A half-named audience refuses. Answering a narrow question with the whole
    // listing would look like a correct answer to a question nobody asked.
    let named = query.audience_kind.is_some() || query.audience_id.is_some();
    let audience = if named {
        match parse_audience(query.audience_kind.as_deref(), query.audience_id.as_deref()) {
            Ok(audience) => Some(audience),
            Err(refusal) => return refusal.response(),
        }
    } else {
        None
    };
    match surface.list_rooms(&scope, audience.as_ref(), Utc::now()) {
        Ok(Ok(listing)) => HttpResponse::Ok().json(listing),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

/// `GET /data-rooms/{room_id}` — one room and every grant on it.
pub async fn get_data_room_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<RoomScopeQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match surface.room(&scope, &path.into_inner(), Utc::now()) {
        Ok(Ok(detail)) => HttpResponse::Ok().json(detail),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

/// `POST /data-rooms/{room_id}/documents` — put a reference in the room.
pub async fn add_data_room_document_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddDocumentBody>,
) -> HttpResponse {
    let scope = match owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let visibility = match parse_visibility(body.visibility.as_deref(), body.identities.as_ref()) {
        Ok(visibility) => visibility,
        Err(refusal) => return refusal.response(),
    };
    let request = AddDocumentRequest {
        room_id: path.into_inner(),
        artifact_ref: body.artifact_ref.clone().unwrap_or_default(),
        visibility,
        added_by: body.added_by.clone().unwrap_or_default(),
    };
    match surface.add_document(&scope, &request, Utc::now()).await {
        Ok(Ok(room)) => HttpResponse::Ok().json(room),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

/// `DELETE /data-rooms/{room_id}/documents?ref=…` — take it out again.
pub async fn withdraw_data_room_document_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<DocumentRefQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let reference = query.reference.clone().unwrap_or_default();
    match surface.withdraw_document(&scope, &path.into_inner(), &reference, Utc::now()) {
        Ok(Ok(room)) => HttpResponse::Ok().json(room),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

/// `POST /data-rooms/{room_id}/links` — issue one identity's capability.
pub async fn issue_data_room_link_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GrantLinkBody>,
) -> HttpResponse {
    grant_response(surface, req, path, body, false).await
}

/// `POST /data-rooms/{room_id}/links/rotate` — replace that credential.
pub async fn rotate_data_room_link_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GrantLinkBody>,
) -> HttpResponse {
    grant_response(surface, req, path, body, true).await
}

async fn grant_response(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GrantLinkBody>,
    rotating: bool,
) -> HttpResponse {
    let scope = match owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let request = GrantLinkRequest {
        room_id: path.into_inner(),
        issued_to: body.issued_to.clone().unwrap_or_default(),
        expires_at: body.expires_at,
        issued_by: body.issued_by.clone().unwrap_or_default(),
    };
    let now = Utc::now();
    let issued = if rotating {
        surface.rotate_link(&scope, &request, now).await
    } else {
        surface.issue_link(&scope, &request, now).await
    };
    match issued {
        Ok(Ok(link)) => HttpResponse::Ok().json(link),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

/// `POST /data-rooms/{room_id}/links/revoke` — kill it.
pub async fn revoke_data_room_link_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RevokeLinkBody>,
) -> HttpResponse {
    let scope = match owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let issued_to = body.issued_to.clone().unwrap_or_default();
    match surface.revoke_link(&scope, &path.into_inner(), &issued_to, Utc::now()) {
        Ok(Ok(revoked)) => HttpResponse::Ok().json(revoked),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

/// `POST /data-rooms/{room_id}/close` — close the room itself.
pub async fn close_data_room_handler(
    surface: web::Data<OwnerSurface>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CloseRoomBody>,
) -> HttpResponse {
    let scope = match owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let closed_by = body.closed_by.clone().unwrap_or_default();
    match surface.close_room(&scope, &path.into_inner(), &closed_by, Utc::now()) {
        Ok(Ok(room)) => HttpResponse::Ok().json(room),
        Ok(Err(refusal)) => refusal.response(),
        Err(error) => unavailable(&error),
    }
}

fn owner_scope(req: &HttpRequest, workspace: Option<String>) -> Result<OwnerScope, HttpResponse> {
    let (principal, workspace) = resolve_required_scope(req.headers(), workspace)?;
    Ok(OwnerScope::new(principal, workspace))
}

/// A store that could not be read or written.
///
/// No detail travels outward — a path on our disk is not an owner's business
/// either — and **nothing is reported as done**: the stores' record-before-act
/// ordering means a failed call left no grant, so saying so is accurate rather
/// than hopeful.
fn unavailable(error: &anyhow::Error) -> HttpResponse {
    tracing::error!(error = ?error, "data room owner surface could not complete a request");
    HttpResponse::ServiceUnavailable().json(serde_json::json!({
        "error": "data_room_unavailable",
        "message": "This request could not be completed and nothing was changed. Try again \
                    shortly.",
    }))
}

/// Mounts the owner routes.
///
/// Registered **inside** `/api/magician/v2`, unlike
/// [`crate::data_room_reader_api`]: every act here is the owner working on
/// their own workspace, so it belongs behind the same gate the rest of the
/// owner surface is behind. The reader is outside that gate because a
/// counterparty holding a capability URL is not a workspace member.
///
/// A second `.route` on the same literal is how this crate registers a second
/// method: `ServiceConfig::route` moves the method guard onto the resource, so
/// a non-matching method falls through to the next registration rather than
/// answering 405 from the first.
pub fn configure_data_room_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/data-rooms", web::get().to(list_data_rooms_handler))
        .route("/data-rooms", web::post().to(open_data_room_handler))
        .route(
            "/data-rooms/{room_id}",
            web::get().to(get_data_room_handler),
        )
        .route(
            "/data-rooms/{room_id}/documents",
            web::post().to(add_data_room_document_handler),
        )
        .route(
            "/data-rooms/{room_id}/documents",
            web::delete().to(withdraw_data_room_document_handler),
        )
        .route(
            "/data-rooms/{room_id}/links",
            web::post().to(issue_data_room_link_handler),
        )
        .route(
            "/data-rooms/{room_id}/links/rotate",
            web::post().to(rotate_data_room_link_handler),
        )
        .route(
            "/data-rooms/{room_id}/links/revoke",
            web::post().to(revoke_data_room_link_handler),
        )
        .route(
            "/data-rooms/{room_id}/close",
            web::post().to(close_data_room_handler),
        );
}

#[cfg(test)]
mod tests {
    //! The grant path's contract, as behaviour.
    //!
    //! Every test names the failure it pins and asserts **values** — a refusal
    //! that is merely "an error" would pass while doing the wrong thing, and a
    //! test that would pass against an empty store proves nothing. So every one
    //! of them seeds a real room, a real document or a real credential and
    //! asserts it EXISTS before asserting it is filtered, refused or killed.

    use super::*;

    use actix_web::{test as actix_test, App};
    use chrono::{Duration, TimeZone};

    use magician::magician_v2::evidence::OutwardActStatus;

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";
    const OTHER_WORKSPACE: &str = "second";
    const OWNER: &str = "owner@ours.test";
    const READER: &str = "reader@acme.test";
    const NEIGHBOUR: &str = "neighbour@acme.test";
    const STRANGER: &str = "stranger@elsewhere.test";
    const DECK: &str = "artifact://deck@3";
    const FINANCIALS: &str = "artifact://financials@1";

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, hour, 0, 0).unwrap()
    }

    fn scope() -> OwnerScope {
        OwnerScope::new(PRINCIPAL, WORKSPACE)
    }

    fn roster(reference: AudienceRef, identities: &[&str]) -> Audience {
        Audience::new(
            reference,
            identities.iter().map(|held| held.to_string()).collect(),
        )
    }

    fn acme() -> Audience {
        roster(AudienceRef::engagement("acme"), &[READER, NEIGHBOUR])
    }

    /// A roster source answering for a fixed set of relationships, and
    /// [`AudienceStanding::Unknown`] for anything else.
    struct StaticRoster(Vec<Audience>);

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
                .iter()
                .find(|audience| audience.reference == *reference)
            {
                Some(audience) => Ok(AudienceStanding::Roster(audience.clone())),
                None => Ok(AudienceStanding::Unknown),
            }
        }
    }

    /// A roster source for a counterparty who is on file and has never been
    /// engaged — the standing that used to reach this surface as an ending.
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

    /// A roster source that cannot be read at all — a permissions fault, a
    /// mount that went away.
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
            anyhow::bail!("the counterparty register could not be read")
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        workspace: ArtifactV2Workspace,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(tmp.path());
            Self {
                _tmp: tmp,
                workspace,
            }
        }

        fn surface_with(&self, rosters: Vec<Audience>) -> OwnerSurface {
            OwnerSurface::new(self.workspace.clone(), Arc::new(StaticRoster(rosters)))
        }

        fn surface(&self) -> OwnerSurface {
            self.surface_with(vec![acme()])
        }

        fn links(&self, room_id: &str) -> Vec<ShareLink> {
            ShareLinkStore::new(self.workspace.clone())
                .for_resource(&ShareLinkScope::new(PRINCIPAL, WORKSPACE), room_id)
                .expect("read the grant log")
        }

        fn rooms(&self) -> Vec<DataRoom> {
            DataRoomStore::new(self.workspace.clone())
                .list(&DataRoomScope::new(PRINCIPAL, WORKSPACE))
                .expect("list the rooms")
        }

        /// Every disclosure the register holds for one recipient, loaded.
        fn disclosures_for(&self, recipient: &str) -> Vec<OutwardActDisclosureRow> {
            let assertions = OutwardAssertionStore::new(self.workspace.clone());
            let outward = OutwardScope::new(PRINCIPAL, WORKSPACE);
            assertions
                .index_entries(&outward, "recipient", recipient)
                .expect("read the recipient index")
                .into_iter()
                .map(|act_ref| {
                    let act = assertions
                        .load_act(&outward, &act_ref)
                        .expect("read the act")
                        .expect("an indexed act exists");
                    OutwardActDisclosureRow {
                        artifact: act.exact_payload_artifact_ref,
                        audience: act.intended_audience,
                        sender: act.effective_sender,
                        status: act.status,
                    }
                })
                .collect()
        }

        /// Make the outward-assertions register unwritable by putting a regular
        /// file where its directory has to be.
        ///
        /// Every path under it then fails with `NotADirectory`, which
        /// `magician_v2::jsonl` propagates rather than folding to "empty" —
        /// which is the whole point: an unreadable register is a fault, and a
        /// fault must stop the grant.
        fn break_the_register(&self) {
            let path = self
                .workspace
                .scope_root(PRINCIPAL, WORKSPACE)
                .join("outward_assertions");
            // A healthy grant earlier in the same test will already have
            // created it as a directory.
            if path.is_dir() {
                std::fs::remove_dir_all(&path).expect("clear the register");
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("scope root");
            }
            std::fs::write(&path, b"not a directory").expect("block the register");
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct OutwardActDisclosureRow {
        artifact: String,
        audience: Vec<String>,
        sender: String,
        status: OutwardActStatus,
    }

    async fn open(surface: &OwnerSurface, audience: AudienceRef, now: DateTime<Utc>) -> RoomView {
        surface
            .open_room(
                &scope(),
                &OpenRoomRequest {
                    audience,
                    opened_by: OWNER.to_string(),
                    closes_at: None,
                },
                now,
            )
            .await
            .expect("storage is healthy")
            .expect("the room opens")
    }

    async fn add(
        surface: &OwnerSurface,
        room_id: &str,
        artifact_ref: &str,
        visibility: DocumentVisibility,
        now: DateTime<Utc>,
    ) -> Result<RoomView, Refusal> {
        surface
            .add_document(
                &scope(),
                &AddDocumentRequest {
                    room_id: room_id.to_string(),
                    artifact_ref: artifact_ref.to_string(),
                    visibility,
                    added_by: OWNER.to_string(),
                },
                now,
            )
            .await
            .expect("storage is healthy")
    }

    fn grant_request(
        room_id: &str,
        issued_to: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> GrantLinkRequest {
        GrantLinkRequest {
            room_id: room_id.to_string(),
            issued_to: issued_to.to_string(),
            expires_at,
            issued_by: OWNER.to_string(),
        }
    }

    // ── The mandatory expiry ────────────────────────────────────────────────

    /// A possession-based grant that never lapses is the blanket-yes failure:
    /// the URL outlives the relationship that justified it. If this surface
    /// picked a deadline for a caller who stated none, the grant would carry a
    /// lifetime nobody agreed to and nobody would ever see the decision. It
    /// must refuse — and refusing must leave no credential behind.
    #[actix_web::test]
    async fn a_link_without_an_expiry_is_refused_rather_than_given_one() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the deck goes in");

        let refusal = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, None),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect_err("an expiry-less grant is refused");

        assert_eq!(refusal.code, "expiry_required");
        assert_eq!(refusal.status, StatusCode::BAD_REQUEST);
        assert!(
            fixture.links(&room.room_id).is_empty(),
            "a refused grant mints nothing"
        );

        // Not vacuous: the identical request WITH an expiry succeeds, so the
        // expiry is the only thing that was missing.
        let issued = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("a grant with an expiry is issued");
        assert_eq!(issued.link.expires_at, at(20));
        assert_eq!(issued.link.state, "live");
        assert_eq!(fixture.links(&room.room_id).len(), 1);
    }

    /// Expiry is inclusive everywhere in this codebase: expiring at eight means
    /// expired at eight. A credential issued with a deadline already reached is
    /// born lapsed, which hides a caller's clock bug behind a URL that silently
    /// opens nothing.
    #[actix_web::test]
    async fn an_expiry_at_the_issuing_instant_is_refused_because_expiry_is_inclusive() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        let refusal = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(10))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a grant born lapsed is refused");
        assert_eq!(refusal.code, "expiry_not_ahead_of_now");
        assert_eq!(refusal.status, StatusCode::BAD_REQUEST);
        assert!(fixture.links(&room.room_id).is_empty());

        // One second later on the clock is a live grant, so the boundary is
        // where the refusal claims it is and not a second either side.
        let issued = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(10) + Duration::seconds(1))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("a grant expiring after now is issued");
        assert_eq!(issued.link.state, "live");
    }

    // ── Recording the disclosure fails the grant ────────────────────────────

    /// The rule this whole surface is arranged around. A grant that succeeded
    /// while its record failed is a disclosure nobody can account for, and no
    /// later sweep could tell that room from one nobody was ever shown. So a
    /// register that cannot be written must leave the counterparty holding
    /// nothing at all.
    #[actix_web::test]
    async fn a_grant_whose_disclosure_cannot_be_recorded_mints_no_credential() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the deck goes in");

        // Not vacuous: with the register healthy the very same call succeeds,
        // for a different member of the same audience.
        let healthy = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, NEIGHBOUR, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("a recordable grant is issued");
        assert_eq!(healthy.disclosures_recorded, 1);
        assert_eq!(fixture.links(&room.room_id).len(), 1);

        fixture.break_the_register();

        let refusal = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(11),
            )
            .await
            .expect("the room and grant stores are still healthy")
            .expect_err("an unrecordable grant is refused");

        assert_eq!(refusal.code, "disclosure_not_recorded");
        assert_eq!(refusal.status, StatusCode::SERVICE_UNAVAILABLE);

        let held = fixture.links(&room.room_id);
        assert_eq!(held.len(), 1, "no second credential was minted");
        assert_eq!(held[0].issued_to, NEIGHBOUR);
    }

    /// A grant records one act per `(present document, admitted holder)` — and
    /// per-document visibility narrows it. If the register listed a document a
    /// holder cannot open, *"who saw this"* would have a wrong answer rather
    /// than a missing one, and a correction would chase somebody who was never
    /// shown the figure.
    #[actix_web::test]
    async fn a_grant_records_only_the_documents_that_holder_may_open() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the deck goes in");
        add(
            &surface,
            &room.room_id,
            FINANCIALS,
            DocumentVisibility::Identities {
                identities: vec![READER.to_string()],
            },
            at(9),
        )
        .await
        .expect("the financials go in, for one reader");

        let neighbour = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, NEIGHBOUR, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("issued");
        let reader = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("issued");

        assert_eq!(neighbour.disclosures_recorded, 1);
        assert_eq!(reader.disclosures_recorded, 2);

        // And the register itself says so, filed under the recipient.
        let told = fixture.disclosures_for(NEIGHBOUR);
        assert_eq!(
            told,
            vec![OutwardActDisclosureRow {
                artifact: DECK.to_string(),
                audience: vec![NEIGHBOUR.to_string()],
                sender: OWNER.to_string(),
                // Accepted, never merely prepared: the disclosure took effect
                // the instant the grant did, so the correction machinery can
                // see it.
                status: OutwardActStatus::ProviderAccepted,
            }]
        );

        let mut reader_artifacts: Vec<String> = fixture
            .disclosures_for(READER)
            .into_iter()
            .map(|row| row.artifact)
            .collect();
        reader_artifacts.sort();
        assert_eq!(
            reader_artifacts,
            vec![DECK.to_string(), FINANCIALS.to_string()]
        );
    }

    /// Adding a document discloses it to everyone holding a **live** credential
    /// — and to nobody else. A revoked holder appearing in the register would
    /// record a disclosure to somebody whose door was already shut, and a live
    /// holder missing from it would be a disclosure nobody could account for.
    #[actix_web::test]
    async fn adding_a_document_discloses_it_to_live_holders_and_not_to_revoked_ones() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        for identity in [READER, NEIGHBOUR] {
            let issued = surface
                .issue_link(
                    &scope(),
                    &grant_request(&room.room_id, identity, Some(at(20))),
                    at(10),
                )
                .await
                .expect("storage is healthy")
                .expect("issued");
            // The room is still empty, so there was nothing to disclose yet.
            assert_eq!(issued.disclosures_recorded, 0);
        }
        let revoked = surface
            .revoke_link(&scope(), &room.room_id, NEIGHBOUR, at(11))
            .expect("storage is healthy")
            .expect("revoked");
        assert_eq!(revoked.revoked_now, 1);

        add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(12),
        )
        .await
        .expect("the deck goes in");

        assert_eq!(
            fixture
                .disclosures_for(READER)
                .into_iter()
                .map(|row| row.artifact)
                .collect::<Vec<_>>(),
            vec![DECK.to_string()],
            "the live holder was disclosed to"
        );
        assert!(
            fixture.disclosures_for(NEIGHBOUR).is_empty(),
            "a revoked holder is not a holder, so nothing was disclosed to them"
        );
    }

    // ── Terminal states, and what a removal may depend on ───────────────────

    /// Revocation is forward-only and terminal. A killed credential must stay
    /// killed — the record of it must not be erased, and a fresh grant in the
    /// same slot must be a **different** credential, or the owner's kill would
    /// be decorative.
    #[actix_web::test]
    async fn a_revoked_credential_stays_dead_and_a_re_issue_is_a_different_secret() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        let first = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("issued");
        assert_eq!(fixture.links(&room.room_id).len(), 1);

        let revoked = surface
            .revoke_link(&scope(), &room.room_id, READER, at(11))
            .expect("storage is healthy")
            .expect("revoked");
        assert_eq!(revoked.revoked_now, 1);
        assert_eq!(revoked.links.len(), 1);
        assert_eq!(revoked.links[0].state, "revoked");
        assert_eq!(revoked.links[0].revoked_at, Some(at(11)));

        // Idempotent: the first revocation IS the revocation, and a second call
        // must not move the recorded time of death.
        let again = surface
            .revoke_link(&scope(), &room.room_id, READER, at(12))
            .expect("storage is healthy")
            .expect("revoked again");
        assert_eq!(again.revoked_now, 0);
        assert_eq!(again.links[0].revoked_at, Some(at(11)));

        let second = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(13),
            )
            .await
            .expect("storage is healthy")
            .expect("a fresh grant after the kill");
        assert_ne!(
            second.secret, first.secret,
            "a killed credential is never re-armed; the replacement is fresh entropy"
        );
        assert_eq!(second.link.link_id, first.link.link_id, "same grant slot");
        assert_eq!(second.link.state, "live");
    }

    /// One identity holds one link, so revocation always has exactly one
    /// target. A second live credential stacked on the same slot would leave a
    /// revocation killing one door while another stayed open — so the caller is
    /// told to rotate, and the credential they already issued is untouched.
    #[actix_web::test]
    async fn a_second_live_grant_to_one_identity_is_refused_and_points_at_rotation() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        let first = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("issued");

        let refusal = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(21))),
                at(11),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a second live grant is refused");
        assert_eq!(refusal.code, "link_already_live");
        assert_eq!(refusal.status, StatusCode::CONFLICT);

        let held = fixture.links(&room.room_id);
        assert_eq!(held.len(), 1);
        assert_eq!(
            held[0].expires_at,
            at(20),
            "the standing grant is untouched"
        );

        // Rotation is the way through, and it replaces the credential in place.
        let rotated = surface
            .rotate_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(21))),
                at(11),
            )
            .await
            .expect("storage is healthy")
            .expect("rotated");
        assert_ne!(rotated.secret, first.secret);
        assert_eq!(rotated.link.link_id, first.link.link_id);
        assert!(rotated.link.rotated, "the successor carries its lineage");
        assert_eq!(rotated.link.expires_at, at(21));
        assert_eq!(fixture.links(&room.room_id).len(), 1, "one slot, not two");
    }

    /// Rotating what does not exist must not quietly become an issue: a caller
    /// who believes an old credential was just invalidated has to find out that
    /// none existed, or they will act as though a leak was closed when nothing
    /// was.
    #[actix_web::test]
    async fn rotating_when_nothing_is_live_refuses_instead_of_issuing() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        let refusal = surface
            .rotate_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect_err("there is nothing to rotate");
        assert_eq!(refusal.code, "no_live_link");
        assert_eq!(refusal.status, StatusCode::CONFLICT);
        assert!(fixture.links(&room.room_id).is_empty());
    }

    /// Access derives from the relationship and ends with it — so a grant into
    /// a dead relationship is refused. A **removal** must not be: revoking a
    /// leaked URL, withdrawing a document and closing a room resolve no roster
    /// at all, or an unreadable register would keep a door open because a
    /// different store was down.
    #[actix_web::test]
    async fn a_relationship_that_ended_blocks_grants_and_never_blocks_removals() {
        let fixture = Fixture::new();
        let live = fixture.surface_with(vec![acme()]);
        let room = open(&live, AudienceRef::engagement("acme"), at(9)).await;
        add(
            &live,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the deck goes in");
        live.issue_link(
            &scope(),
            &grant_request(&room.room_id, READER, Some(at(23))),
            at(10),
        )
        .await
        .expect("storage is healthy")
        .expect("issued while the relationship was current");

        let ended = fixture.surface_with(vec![acme().expiring_at(at(12))]);

        let add_refusal = add(
            &ended,
            &room.room_id,
            FINANCIALS,
            DocumentVisibility::Everyone,
            at(13),
        )
        .await
        .expect_err("a grant into a dead relationship is refused");
        assert_eq!(add_refusal.code, "relationship_ended");
        assert_eq!(add_refusal.status, StatusCode::CONFLICT);

        let issue_refusal = ended
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, NEIGHBOUR, Some(at(23))),
                at(13),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a credential into a dead relationship is refused");
        assert_eq!(issue_refusal.code, "relationship_ended");

        // Removals go through, on the same dead relationship.
        let revoked = ended
            .revoke_link(&scope(), &room.room_id, READER, at(13))
            .expect("storage is healthy")
            .expect("a revocation never asks about the relationship");
        assert_eq!(revoked.revoked_now, 1);

        let withdrawn = ended
            .withdraw_document(&scope(), &room.room_id, DECK, at(13))
            .expect("storage is healthy")
            .expect("a withdrawal never asks about the relationship");
        assert_eq!(withdrawn.documents_present, 0);

        let closed = ended
            .close_room(&scope(), &room.room_id, OWNER, at(13))
            .expect("storage is healthy")
            .expect("closing never asks about the relationship");
        assert_eq!(closed.standing, "closed");
    }

    /// A relationship that was **never recorded** is not one that ended, and
    /// the owner is not told it was.
    ///
    /// Both refuse and neither grants, so no test of *whether the room opens*
    /// can tell the two apart — which is exactly how they came to render
    /// identically. The difference is the only thing the owner can act on: the
    /// `relationship_ended` message names revoke, withdraw and close, and not
    /// one of those three can bring into being a relationship nobody recorded.
    /// The message here has to name the act that would.
    #[actix_web::test]
    async fn a_relationship_that_was_never_recorded_is_not_reported_as_one_that_ended() {
        let fixture = Fixture::new();
        let never = OwnerSurface::new(fixture.workspace.clone(), Arc::new(NeverEngagedRoster));

        let refusal = never
            .open_room(
                &scope(),
                &OpenRoomRequest {
                    audience: AudienceRef::engagement("acme"),
                    opened_by: OWNER.to_string(),
                    closes_at: None,
                },
                at(9),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a relationship nothing records opens no room");

        assert_eq!(
            refusal.code, "relationship_not_started",
            "`relationship_ended` here tells the owner something finished that never began, \
             and `relationship_unknown` loses the one detail that says the register is fine \
             and only the engagement is missing"
        );
        assert_eq!(refusal.status, StatusCode::CONFLICT);
        assert!(
            !refusal.message.contains("revoke")
                && !refusal.message.contains("withdraw")
                && !refusal.message.contains("close"),
            "the message steers the owner at the three acts that cannot help: {}",
            refusal.message
        );
        assert!(
            refusal.message.contains("Record the engagement"),
            "the message has to name the act that would help: {}",
            refusal.message
        );
        assert!(
            fixture.rooms().is_empty(),
            "and it fails closed — no room was opened on a relationship nothing records"
        );
    }

    /// Closing is terminal. Handing a closed room back to somebody who asked to
    /// open one would read as success, and they would go on to share documents
    /// into a room nobody can read.
    #[actix_web::test]
    async fn a_closed_room_never_reopens_and_grants_nothing() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        let closed = surface
            .close_room(&scope(), &room.room_id, OWNER, at(10))
            .expect("storage is healthy")
            .expect("closed");
        assert_eq!(closed.standing, "closed");
        assert_eq!(closed.closed_at, Some(at(10)));

        let reopen = surface
            .open_room(
                &scope(),
                &OpenRoomRequest {
                    audience: AudienceRef::engagement("acme"),
                    opened_by: OWNER.to_string(),
                    closes_at: None,
                },
                at(11),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a closed room does not re-open");
        assert_eq!(reopen.code, "room_closed");
        assert_eq!(reopen.status, StatusCode::CONFLICT);

        let add_refusal = add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(11),
        )
        .await
        .expect_err("a closed room takes no documents");
        assert_eq!(add_refusal.code, "room_closed");

        let issue_refusal = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(11),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a closed room issues nothing");
        assert_eq!(issue_refusal.code, "room_closed");
        assert!(fixture.links(&room.room_id).is_empty());
    }

    // ── Replay resumes; a changed payload is an error ───────────────────────

    /// `DataRoomStore::open` returns the existing room, which makes a retry
    /// resume — and would make a caller asking for a **different** closing date
    /// believe they had moved a date that never moved. An identical replay must
    /// resume; a changed one must be an error rather than a silent no-op.
    #[actix_web::test]
    async fn reopening_on_different_terms_is_an_error_and_an_identical_replay_resumes() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let request = |closes_at: Option<DateTime<Utc>>| OpenRoomRequest {
            audience: AudienceRef::engagement("acme"),
            opened_by: OWNER.to_string(),
            closes_at,
        };

        let first = surface
            .open_room(&scope(), &request(Some(at(20))), at(9))
            .await
            .expect("storage is healthy")
            .expect("opened");
        assert_eq!(first.closes_at, Some(at(20)));

        let resumed = surface
            .open_room(&scope(), &request(Some(at(20))), at(10))
            .await
            .expect("storage is healthy")
            .expect("an identical replay resumes");
        assert_eq!(resumed.room_id, first.room_id);
        assert_eq!(resumed.opened_at, at(9), "the original opening stands");

        let refusal = surface
            .open_room(&scope(), &request(Some(at(21))), at(10))
            .await
            .expect("storage is healthy")
            .expect_err("a changed closing date is an error");
        assert_eq!(refusal.code, "room_already_open_on_other_terms");
        assert_eq!(refusal.status, StatusCode::CONFLICT);
        assert_eq!(
            fixture.rooms()[0].closes_at,
            Some(at(20)),
            "and the stored date did not move"
        );
    }

    /// An unpinned reference is the CALLER's fault, and must read as one.
    ///
    /// The store refuses it too, but its refusal is an `anyhow::Error` that the
    /// surface propagates with `?` and the handler renders as a 503 saying
    /// *"Try again shortly."* Retrying an unpinned reference fails identically
    /// forever, so the caller was told to do the one thing that cannot work —
    /// while an operator watching 503s went looking at a healthy store.
    #[test]
    fn a_reference_naming_no_revision_is_a_bad_request_not_an_outage() {
        let refusal = super::pinned_reference_refusal("artifact://deck")
            .expect_err("an unpinned reference is refused");
        assert_eq!(refusal.code, "unpinned_document_reference");
        assert_eq!(
            refusal.status,
            StatusCode::BAD_REQUEST,
            "a 503 would tell the caller to retry something that can never succeed"
        );
        assert!(
            refusal.message.contains("artifact://deck@<revision>"),
            "the refusal has to carry the remedy: {}",
            refusal.message
        );

        let ambiguous = super::pinned_reference_refusal("mailto:a@b.test@v9")
            .expect_err("two separators cannot be split reliably");
        assert_eq!(ambiguous.code, "ambiguous_document_reference");
        assert_eq!(ambiguous.status, StatusCode::BAD_REQUEST);

        super::pinned_reference_refusal("artifact://deck@r2")
            .expect("exactly one separator is the form the store admits");
    }

    /// The store is idempotent on the artifact reference and returns the room
    /// unchanged when the document is already there — including when the caller
    /// asked for a NARROWER visibility. Accepting that call would report
    /// success for a restriction that was never applied, and the owner would
    /// believe a document was restricted while the whole audience could open it.
    #[actix_web::test]
    async fn re_adding_a_document_under_a_different_visibility_is_refused_not_ignored() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the deck goes in");

        let refusal = add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Identities {
                identities: vec![READER.to_string()],
            },
            at(10),
        )
        .await
        .expect_err("a narrowing re-add is refused");
        assert_eq!(refusal.code, "document_visibility_conflict");
        assert_eq!(refusal.status, StatusCode::CONFLICT);

        let rooms = fixture.rooms();
        let held = &rooms[0].documents[0];
        assert_eq!(held.artifact_ref, DECK);
        assert!(
            matches!(held.visibility, DocumentVisibility::Everyone),
            "the stored visibility did not quietly change"
        );

        // An identical re-add still resumes, so the refusal is about the
        // CHANGE and not about repetition.
        let resumed = add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(10),
        )
        .await
        .expect("an identical re-add resumes");
        assert_eq!(resumed.documents_present, 1);
        assert_eq!(resumed.documents.len(), 1, "and did not list it twice");
    }

    /// A withdrawal takes a document out of the room and leaves it in the
    /// history. *"This was in the room between March and April"* is the question
    /// an audit asks, and a deleted row cannot answer it.
    #[actix_web::test]
    async fn withdrawing_keeps_the_entry_and_takes_it_out_of_the_room() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        let with_deck = add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the deck goes in");
        assert_eq!(with_deck.documents_present, 1);

        let withdrawn = surface
            .withdraw_document(&scope(), &room.room_id, DECK, at(11))
            .expect("storage is healthy")
            .expect("withdrawn");
        assert_eq!(withdrawn.documents_present, 0);
        assert_eq!(withdrawn.documents.len(), 1, "the entry is kept");
        assert!(!withdrawn.documents[0].present);
        assert_eq!(withdrawn.documents[0].withdrawn_at, Some(at(11)));

        // A removal that has already happened is a no-op success: the end state
        // the caller asked for already holds.
        let again = surface
            .withdraw_document(&scope(), &room.room_id, DECK, at(12))
            .expect("storage is healthy")
            .expect("withdrawing twice is a no-op");
        assert_eq!(
            again.documents[0].withdrawn_at,
            Some(at(11)),
            "the recorded time of the withdrawal did not move"
        );
    }

    // ── Fail closed on unknown, absent and unreadable ───────────────────────

    /// Unknown is never permission. A room opened against a relationship nobody
    /// holds could never be read — and would come alive the day somebody
    /// registered that id, without anyone deciding to share anything.
    #[actix_web::test]
    async fn an_unknown_relationship_opens_no_room() {
        let fixture = Fixture::new();
        let surface = fixture.surface_with(vec![acme()]);

        let refusal = surface
            .open_room(
                &scope(),
                &OpenRoomRequest {
                    audience: AudienceRef::engagement("nobody"),
                    opened_by: OWNER.to_string(),
                    closes_at: None,
                },
                at(9),
            )
            .await
            .expect("storage is healthy")
            .expect_err("an unknown relationship opens nothing");
        assert_eq!(refusal.code, "relationship_unknown");
        assert_eq!(refusal.status, StatusCode::NOT_FOUND);
        assert!(fixture.rooms().is_empty());

        // Not vacuous: the relationship that IS held opens a room.
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        assert_eq!(fixture.rooms().len(), 1);
        assert_eq!(fixture.rooms()[0].room_id, room.room_id);
    }

    /// An unreadable roster is a fault, never an empty audience. Folding it to
    /// "nobody is in this relationship" would let a room be opened, or a
    /// document added, on the strength of a store that was simply down.
    #[actix_web::test]
    async fn an_unreadable_roster_is_a_fault_and_never_an_empty_audience() {
        let fixture = Fixture::new();
        let surface = OwnerSurface::new(fixture.workspace.clone(), Arc::new(UnreadableRoster));

        let error = surface
            .open_room(
                &scope(),
                &OpenRoomRequest {
                    audience: AudienceRef::engagement("acme"),
                    opened_by: OWNER.to_string(),
                    closes_at: None,
                },
                at(9),
            )
            .await
            .expect_err("an unreadable roster is a fault");
        assert!(
            error.to_string().contains("could not be read"),
            "the fault is reported as itself: {error}"
        );
        assert!(
            fixture.rooms().is_empty(),
            "and no room was opened on the strength of it"
        );
    }

    /// An audience naming nobody admits nobody. A credential issued to somebody
    /// outside the roster opens nothing today and would come alive the day they
    /// were added — a grant nobody decided to make — so it is refused now.
    #[actix_web::test]
    async fn a_credential_for_an_identity_the_roster_does_not_name_is_refused() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        let refusal = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, STRANGER, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a stranger gets no credential");
        assert_eq!(refusal.code, "identity_not_admitted");
        assert_eq!(refusal.status, StatusCode::FORBIDDEN);
        assert!(fixture.links(&room.room_id).is_empty());

        // Not vacuous: somebody the roster DOES name is granted through the
        // same call.
        surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, READER, Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect("an admitted identity is granted");
        assert_eq!(fixture.links(&room.room_id).len(), 1);
    }

    /// `U+001F` is the separator every derived id in this set is joined with. A
    /// caller string carrying one could shift bytes across it and fuse two
    /// people's disclosures into a single record — after which *"who saw
    /// this"* has a wrong answer rather than a missing one.
    #[actix_web::test]
    async fn a_caller_string_carrying_the_unit_separator_is_refused_everywhere() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        let refusal = surface
            .issue_link(
                &scope(),
                &grant_request(&room.room_id, "reader\u{1f}other@acme.test", Some(at(20))),
                at(10),
            )
            .await
            .expect("storage is healthy")
            .expect_err("a separator-carrying identity is refused");
        assert_eq!(refusal.code, "malformed_request");
        assert_eq!(refusal.status, StatusCode::BAD_REQUEST);
        assert!(fixture.links(&room.room_id).is_empty());

        let document = add(
            &surface,
            &room.room_id,
            "artifact://deck\u{1f}@3",
            DocumentVisibility::Everyone,
            at(10),
        )
        .await
        .expect_err("a separator-carrying reference is refused");
        assert_eq!(document.code, "malformed_request");
        assert_eq!(
            fixture.rooms()[0].documents.len(),
            0,
            "and nothing was added"
        );
    }

    /// A room id that is not one this store could have minted never reaches the
    /// store, which interpolates it into a file name under the authenticated
    /// scope.
    #[actix_web::test]
    async fn a_room_id_that_is_not_a_derived_id_never_reaches_the_store() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        for candidate in ["../../etc/passwd", "room-not-hex", "", &room.room_id[1..]] {
            let refusal = surface
                .room(&scope(), candidate, at(10))
                .expect("storage is healthy")
                .expect_err("a non-derived id is refused");
            assert_eq!(refusal.code, "malformed_room_id", "for `{candidate}`");
            assert_eq!(refusal.status, StatusCode::BAD_REQUEST);
        }

        // Not vacuous: the id the store actually minted resolves.
        let detail = surface
            .room(&scope(), &room.room_id, at(10))
            .expect("storage is healthy")
            .expect("the real room resolves");
        assert_eq!(detail.room.room_id, room.room_id);
    }

    // ── Generic by construction ─────────────────────────────────────────────

    /// The audience KIND is part of a room's identity. The same organisation as
    /// a live deal and as a standing client must be two rooms, or one flow's
    /// documents would appear in the other's — and this is the specific way
    /// widening the binding from *engagement* to *audience* could have gone
    /// wrong.
    #[actix_web::test]
    async fn one_id_under_two_audience_kinds_is_two_rooms() {
        let fixture = Fixture::new();
        let surface = fixture.surface_with(vec![
            roster(AudienceRef::panel("acme"), &[READER]),
            roster(AudienceRef::account("acme"), &[NEIGHBOUR]),
        ]);

        let panel = open(&surface, AudienceRef::panel("acme"), at(9)).await;
        let account = open(&surface, AudienceRef::account("acme"), at(9)).await;

        assert_ne!(panel.room_id, account.room_id);
        assert_eq!(panel.audience_kind, "panel");
        assert_eq!(account.audience_kind, "account");
        assert_eq!(panel.audience_id, "acme");

        let listing = surface
            .list_rooms(&scope(), None, at(10))
            .expect("storage is healthy")
            .expect("listed");
        assert_eq!(listing.count, 2);

        // And a filtered listing answers with the ONE room that audience has.
        let filtered = surface
            .list_rooms(&scope(), Some(&AudienceRef::panel("acme")), at(10))
            .expect("storage is healthy")
            .expect("listed");
        assert_eq!(filtered.count, 1);
        assert_eq!(filtered.rooms[0].room_id, panel.room_id);
    }

    /// A kind this surface does not know is refused rather than defaulted.
    /// Parsing that silently answered `engagement` would file a panel's room,
    /// an account's or a person's under the engagement key — and the key exists
    /// precisely so those never merge.
    #[test]
    fn an_unknown_audience_kind_is_refused_and_a_known_one_is_case_folded() {
        let refusal =
            parse_audience(Some("cohort"), Some("acme")).expect_err("`cohort` is not a kind");
        assert_eq!(refusal.code, "unknown_audience_kind");
        assert_eq!(refusal.status, StatusCode::BAD_REQUEST);

        let half = parse_audience(Some("engagement"), None).expect_err("half a name is no name");
        assert_eq!(half.code, "audience_required");

        // Every arm this codebase declares is nameable from a route, and case
        // is folded because a kind typed on two different days is one kind.
        for kind in AudienceKind::ALL {
            let parsed = parse_audience(Some(&kind.as_str().to_ascii_uppercase()), Some("acme"))
                .expect("every declared kind is nameable");
            assert_eq!(parsed, AudienceRef::new(kind, "acme"));
        }
    }

    /// Visibility has no default. An absent one reading as `everyone` would
    /// widen a grant nobody typed, and the widening would be invisible in the
    /// request that made it.
    #[test]
    fn visibility_is_never_defaulted_and_an_empty_restriction_is_refused() {
        assert_eq!(
            parse_visibility(None, None).expect_err("no default").code,
            "visibility_required"
        );
        assert_eq!(
            parse_visibility(Some("public"), None)
                .expect_err("not a visibility")
                .code,
            "unknown_visibility"
        );
        let nobody: Vec<String> = Vec::new();
        assert_eq!(
            parse_visibility(Some("identities"), Some(&nobody))
                .expect_err("a restriction naming nobody")
                .code,
            "identities_required"
        );
        assert_eq!(
            parse_visibility(Some("everyone"), Some(&vec![READER.to_string()]))
                .expect_err("naming identities under `everyone` reads as a restriction")
                .code,
            "visibility_conflict"
        );

        // And the two it does accept say exactly what they say — with a
        // restriction normalised, so the same list typed twice is one list.
        assert_eq!(
            parse_visibility(Some(" Everyone "), None).expect("accepted"),
            DocumentVisibility::Everyone
        );
        assert_eq!(
            parse_visibility(
                Some("identities"),
                Some(&vec![
                    NEIGHBOUR.to_string(),
                    READER.to_string(),
                    NEIGHBOUR.to_string(),
                ])
            )
            .expect("accepted"),
            DocumentVisibility::Identities {
                identities: vec![NEIGHBOUR.to_string(), READER.to_string()],
            }
        );
    }

    // ── Scope isolation ─────────────────────────────────────────────────────

    /// The listing is the owner's answer to *"what have I shared"*. A listing
    /// that leaked another tenant's rooms would be worse than no listing at
    /// all, and one that hid a room would tell an owner they have shared less
    /// than they have.
    #[actix_web::test]
    async fn the_listing_names_every_room_in_the_scope_and_nothing_from_another() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let here = open(&surface, AudienceRef::engagement("acme"), at(9)).await;

        let other = OwnerScope::new(PRINCIPAL, OTHER_WORKSPACE);
        let there = surface
            .open_room(
                &other,
                &OpenRoomRequest {
                    audience: AudienceRef::engagement("acme"),
                    opened_by: OWNER.to_string(),
                    closes_at: None,
                },
                at(9),
            )
            .await
            .expect("storage is healthy")
            .expect("opened in the other workspace");
        assert_ne!(
            here.room_id, there.room_id,
            "the scope is folded into the room id"
        );

        let mine = surface
            .list_rooms(&scope(), None, at(10))
            .expect("storage is healthy")
            .expect("listed");
        assert_eq!(mine.count, 1);
        assert_eq!(mine.rooms[0].room_id, here.room_id);

        let theirs = surface
            .list_rooms(&other, None, at(10))
            .expect("storage is healthy")
            .expect("listed");
        assert_eq!(theirs.count, 1);
        assert_eq!(theirs.rooms[0].room_id, there.room_id);
    }

    /// A room's counts are what an owner scans a listing for: how much is in
    /// it, and how many people can currently open it. A live count including a
    /// revoked or lapsed credential would read as access that is not there.
    #[actix_web::test]
    async fn a_room_view_counts_present_documents_and_live_credentials() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let room = open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        add(
            &surface,
            &room.room_id,
            DECK,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the deck goes in");
        add(
            &surface,
            &room.room_id,
            FINANCIALS,
            DocumentVisibility::Everyone,
            at(9),
        )
        .await
        .expect("the financials go in");
        surface
            .withdraw_document(&scope(), &room.room_id, FINANCIALS, at(9))
            .expect("storage is healthy")
            .expect("withdrawn");

        for identity in [READER, NEIGHBOUR] {
            surface
                .issue_link(
                    &scope(),
                    &grant_request(&room.room_id, identity, Some(at(20))),
                    at(10),
                )
                .await
                .expect("storage is healthy")
                .expect("issued");
        }
        surface
            .revoke_link(&scope(), &room.room_id, NEIGHBOUR, at(11))
            .expect("storage is healthy")
            .expect("revoked");

        let detail = surface
            .room(&scope(), &room.room_id, at(12))
            .expect("storage is healthy")
            .expect("read");
        assert_eq!(detail.room.documents_present, 1);
        assert_eq!(detail.room.documents.len(), 2, "history is kept");
        assert_eq!(detail.room.live_links, 1);
        assert_eq!(detail.room.links_ever, 2);
        assert_eq!(detail.links.len(), 2, "dead grants stay readable");

        // Derived from the clock, never stored: past the expiry the same rows
        // read as expired with nothing having run in between.
        let later = surface
            .room(&scope(), &room.room_id, at(20))
            .expect("storage is healthy")
            .expect("read");
        assert_eq!(later.room.live_links, 0);
        let mut states: Vec<&str> = later.links.iter().map(|link| link.state).collect();
        states.sort_unstable();
        assert_eq!(states, vec!["expired", "revoked"]);
    }

    // ── The routes ──────────────────────────────────────────────────────────

    /// The whole point of this module is that the grant path is REACHABLE. A
    /// surface that compiled and was mounted nowhere is how the stores it calls
    /// came to have no production caller in the first place — so the routes are
    /// exercised through a real app.
    #[actix_web::test]
    async fn the_routes_open_a_room_grant_a_link_and_revoke_it() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(surface))
                .configure(configure_data_room_routes),
        )
        .await;

        let opened: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri("/data-rooms")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .set_json(serde_json::json!({
                    "audience_kind": "engagement",
                    "audience_id": "acme",
                    "opened_by": OWNER,
                }))
                .to_request(),
        )
        .await;
        let room_id = opened["room_id"].as_str().expect("a room id").to_string();
        assert_eq!(opened["audience_kind"], "engagement");
        assert_eq!(opened["standing"], "open");

        let added: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri(&format!("/data-rooms/{room_id}/documents"))
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .set_json(serde_json::json!({
                    "artifact_ref": DECK,
                    "added_by": OWNER,
                    "visibility": "everyone",
                }))
                .to_request(),
        )
        .await;
        assert_eq!(added["documents_present"], 1);

        let issued: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri(&format!("/data-rooms/{room_id}/links"))
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                // The handler reads the wall clock, so the expiry has to be
                // ahead of the real one — a fixture hour would be refused as a
                // grant born lapsed, which is the rule working.
                .set_json(serde_json::json!({
                    "issued_to": READER,
                    "issued_by": OWNER,
                    "expires_at": Utc::now() + Duration::days(1),
                }))
                .to_request(),
        )
        .await;
        assert_eq!(issued["disclosures_recorded"], 1);
        assert_eq!(issued["link"]["state"], "live");
        assert_eq!(
            issued["secret"].as_str().expect("a secret").len(),
            SECRET_BYTES * 2,
            "the credential is full-width entropy, hex-encoded"
        );

        let revoked: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri(&format!("/data-rooms/{room_id}/links/revoke"))
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .set_json(serde_json::json!({ "issued_to": READER }))
                .to_request(),
        )
        .await;
        assert_eq!(revoked["revoked_now"], 1);
        assert_eq!(revoked["links"][0]["state"], "revoked");
    }

    /// A route that answered a narrow question with the whole listing would
    /// look like a correct answer to a question nobody asked. A half-named
    /// audience filter refuses instead.
    #[actix_web::test]
    async fn a_half_named_audience_filter_refuses_rather_than_listing_everything() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        open(&surface, AudienceRef::engagement("acme"), at(9)).await;
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(surface))
                .configure(configure_data_room_routes),
        )
        .await;

        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/data-rooms?audience_kind=engagement")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "audience_required");

        // Not vacuous: with no filter at all the same route lists the room
        // that does exist.
        let listing: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/data-rooms")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(listing["count"], 1);
    }
}
