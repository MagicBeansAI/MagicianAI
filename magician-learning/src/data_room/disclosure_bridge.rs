//! Making a document visible **is** a disclosure — the Data Room's write point
//! into the outward-assertions register.
//!
//! Outward Assertions plan phase 3
//! (`docs/plans/2026-08-07-opc-outward-assertions.md`): the plan's §5 table
//! assigns *"a data-room revision becomes visible"* to the Data Room, and that
//! write point stayed blocked on a Data Room existing at all. The room exists
//! now (deal-close plan, `super`); this is the bridge.
//!
//! # What one disclosure records
//!
//! The register's own contract: *"that a claim was asserted, to these people,
//! through this exact artifact revision, on this channel, at this time."*
//! Mapped onto a room: granting an identity visibility of a document — a live
//! share link exists, the document is present, and its visibility admits that
//! identity — is one act per `(document, holder)` pair, on
//! [`OutwardChannel::Room`], with the holder as the entire intended audience.
//! Per pair rather than per room, because correction propagation aims at
//! people: a corrected figure must find *who* saw the artifact that stated it,
//! not the container it sat in.
//!
//! # Prepare, advance, never observe
//!
//! The write fires when visibility is **granted** — before any access. That is
//! the store's §4 ordering: prepare before the act, and if preparation fails
//! the act must fail closed (the caller must not expose the document).
//! [`OutwardChannel::Room`] is a **controlled** channel — we own the gate, and
//! nothing is readable until we grant it — so the prepare ladder is the
//! correct entry (via [`OutwardAssertionStore::prepare_for_audience`], which
//! prepares and then files the act under its audience's axis — see below), and
//! `record_observed_act` is never correct here: an observed record preserves
//! the uncertainty of words that had already left before anything could be
//! recorded, which is not this situation.
//!
//! `Prepared` is not where a grant may rest, though. The status taxonomy reads
//! `Prepared` as *"recorded, nothing has left yet"*, and the correction
//! machinery agrees: `is_active_disclosure` excludes it, so a corrected claim
//! raises no obligation against a `Prepared` act. That is right for email,
//! where dispatch is a later step that can still fail, and wrong for a room,
//! where the disclosure takes effect the instant the grant does. So each act
//! is advanced in the same call — `Dispatching`, then `ProviderAccepted` with
//! the grant itself as the effect receipt: the room is a provider we own, and
//! it accepted the disclosure the moment the door opened. **Not** `Delivered`
//! — acceptance is not delivery, and whether anyone walked through the door is
//! the access log's knowledge; the caller may `record_delivered` when a first
//! real presentation is observed.
//!
//! # Consequence class
//!
//! Every act here is [`ConsequenceClass::ConfidentialDisclosure`] — the
//! envelopes plan's §3 names *"financials, a data room"* as the class's own
//! example. Carried, not interpreted: the envelope resolver decides what may
//! be done about an act of this class; this bridge only records what the act
//! was.
//!
//! # Which axis a grant is filed under, and the id it must never claim
//!
//! A room's [`AudienceRef`](magician::magician_v2::audience::AudienceRef) is a
//! kind and an id, and the id belongs to the kind's own space: for a room to
//! open at all its audience must resolve through the counterparty register
//! (`CounterpartyAudiences::living_audience` loads it with
//! `counterparty_store.load`), so what the room holds is a **counterparty** id.
//! `OutwardActDisclosure::engagement_id` names something else — the engagement
//! an act was performed *inside*, which is what `WorkContextKind::Engagement`
//! carries and what every reverse lookup under the engagement axis asks with.
//!
//! An earlier cut copied one into the other whenever the kinds' names matched.
//! Every act it wrote was a false statement of fact at write time, and the
//! store's indexes then served that statement as truth — including to
//! `reindex_work_axes`, which counted those acts as *attributed* and so gave a
//! confidently wrong answer to the one question it exists for. Worse in the
//! other direction: an `Account`, `Panel` or `Person` room has no work field it
//! could honestly fill, so its grants were filed under no relationship at all.
//!
//! So the audience travels whole to
//! [`OutwardAssertionStore::prepare_for_audience`], which derives the axis from
//! the KIND by an exhaustive match — a sixth audience kind fails to compile
//! rather than filing under the wrong axis or under none — and the two work
//! fields are left empty, because this bridge genuinely does not know a work.
//!
//! # What this module refuses to own, and why
//!
//! - **Anything at all.** It has no store, no log, no state. Pure
//!   orchestration over supplied state and a store handle; the acts live in
//!   the evidence subsystem, which the plan's §2 fixes as the one
//!   authoritative copy.
//! - **The share-link machinery.** Holders arrive as a plain slice of
//!   identities, supplied by whoever knows who currently holds a live link.
//!   Reading a link store here would couple every caller to that one source; a
//!   slice keeps the write point usable by any flow that can answer *"who can
//!   get in right now"*.
//! - **Access recording.** This records the *grant*. What a reader actually
//!   opened, and when, is the access log's job (`super::access_log`) at read
//!   time. Conflating the two would let "we showed it to them" be inferred
//!   from "they looked", which is backwards — the disclosure happened when the
//!   door opened, whether or not anyone walked through.
//! - **Revision enforcement.** `exact_payload_artifact_ref` is the entry's
//!   `artifact_ref` — the reference the room holds instead of a copy
//!   (deal-close plan §4). Whether that reference pins an immutable revision
//!   is inherited from the room's reference-not-copy rule, not re-checked
//!   here; a room pointing at a mutable "latest" would weaken the record, and
//!   the fix belongs where the reference is created, not in every consumer.

use std::collections::HashSet;

use anyhow::Result;
use chrono::{DateTime, Utc};

use magician::magician_v2::agents::ConsequenceClass;
use magician::magician_v2::audience::Audience;
use magician::magician_v2::evidence::{
    OutwardActDisclosure, OutwardActStatus, OutwardAssertionStore, OutwardChannel, OutwardScope,
    PrepareOutwardAct,
};

use super::types::DataRoom;

const FIELD_SEP: char = '\u{1f}';

/// Record one disclosure per `(present document, link holder)` pair the room
/// currently shows — the room-visibility write point.
///
/// Call this **when visibility is granted and before it takes effect**. On
/// `Err` the caller must fail closed and not expose anything: an exposure that
/// succeeded while its record failed is a disclosure nobody can find later,
/// which is exactly the state the register exists to make impossible (§4).
/// Acts already recorded before an error are durable and harmless — a retry
/// resumes each from whatever rung it reached, never duplicating a record or
/// a transition.
///
/// Returns every disclosure recorded for this grant, in the room's document
/// order and then first-seen holder order, so the caller can see exactly what
/// was recorded. Each rests at `ProviderAccepted` with the grant as its
/// effect receipt (see the module note on advancing) — never `Delivered`:
/// first actual presentation is the access log's fact, and the caller may
/// `record_delivered` on the act when it observes one.
///
/// # Inputs are supplied, not discovered
///
/// - `holders` — the identities holding **live** share links, supplied by the
///   caller from whatever issues links. Deliberately a plain slice: depending
///   on a link store would couple this write point to one flow, and any caller
///   that can enumerate current holders may use it. Duplicates collapse to the
///   first occurrence, so a sloppy caller list cannot double-record a grant.
/// - `audience` — the roster of the relationship the room was opened for,
///   supplied by whoever owns that relationship. It must be *that*
///   relationship's roster: a mismatch is an error, never an empty result,
///   because a wrong roster is a caller bug, and recording against it would
///   attribute confidential disclosures to the wrong relationship.
///
/// # Two clocks, two forms, one instant
///
/// `now` and `now_rfc3339` are the same moment in two forms, both supplied by
/// the caller. Room standing and per-document visibility are typed-clock
/// checks; the assertions store speaks RFC 3339 strings end to end. Deriving
/// one form from the other inside this module would mean either calling
/// `Utc::now()` in library code (the clock is always the caller's) or
/// round-tripping through a parse this module does not own — and a formatting
/// difference there would silently change what the permanent record says the
/// time was.
///
/// # What produces nothing, and what refuses
///
/// - A room that is not open discloses **nothing**. Checked first, before the
///   roster is even looked at: a closed room shows nobody anything, so there
///   is no roster question left to be wrong about.
/// - A holder the audience does not admit — never listed, or the relationship
///   has ended — simply produces no acts: absence of access is absence of
///   disclosure.
/// - An empty holder list produces an empty result: no live link, no grant,
///   nothing to record. Every act that *is* produced names exactly one
///   recipient — `intended_audience` is never empty — so no act from this
///   bridge can vacuously satisfy a recipient predicate downstream.
#[allow(clippy::too_many_arguments)]
pub fn record_room_disclosures(
    assertions: &OutwardAssertionStore,
    outward_scope: &OutwardScope,
    room: &DataRoom,
    audience: &Audience,
    holders: &[String],
    disclosed_by: &str,
    now: DateTime<Utc>,
    now_rfc3339: &str,
) -> Result<Vec<OutwardActDisclosure>> {
    if room.room_id.trim().is_empty() {
        anyhow::bail!(
            "a room with no id cannot anchor a disclosure: the idempotency key is derived from \
             the room id, and a blank one would merge grants made through different rooms into \
             one record, corrupting the register's answer to who saw what"
        );
    }
    if disclosed_by.trim().is_empty() {
        anyhow::bail!(
            "a disclosure must name who granted it: `effective_sender` is how the register \
             answers *who told them*, and a blank sender would record an assertion nobody made"
        );
    }

    // Standing first, before the roster is examined: a closed or expired room
    // discloses nothing to anyone, whoever's roster arrives with it.
    if !room.standing(now).is_open() {
        return Ok(Vec::new());
    }

    if audience.reference != room.audience {
        anyhow::bail!(
            "roster is for `{}` but the room was opened for `{}`: a wrong roster is a caller \
             bug, not an absence — recording against it would attribute confidential \
             disclosures to the wrong relationship, and returning empty would hide the bug",
            audience.reference.as_key(),
            room.audience.as_key()
        );
    }

    // First occurrence wins; a repeated holder in the caller's list is one
    // grant. The store's derived act ref would collapse the duplicate anyway,
    // but the returned list must not show one grant twice either.
    let mut seen = HashSet::new();
    let holders: Vec<&str> = holders
        .iter()
        .map(String::as_str)
        .filter(|holder| seen.insert(*holder))
        .collect();

    // The audience index, read ONCE. `prepare_for_audience` reads this whole
    // file to answer one containment question, which is fine for a single act
    // and quadratic here: this loop prepares one act per
    // `(present document × live holder)` pair, and the file it would re-read
    // every time is the one the loop is growing.
    let mut filed = assertions.audience_index_set(outward_scope, &room.audience)?;

    let mut disclosures = Vec::new();
    for document in room.present_documents() {
        for &holder in &holders {
            // `permits` checks audience membership first — including the
            // relationship still being current — then the per-document rule.
            // Anyone it refuses has not been shown the document, so there is
            // nothing to record for them.
            if !document.visibility.permits(holder, audience, now) {
                continue;
            }
            let request = PrepareOutwardAct {
                idempotency_key: disclosure_idempotency_key(
                    &room.room_id,
                    &document.artifact_ref,
                    holder,
                ),
                // Neither work field, ever. A room's audience id names the
                // relationship the room was opened for — a counterparty id,
                // because `CounterpartyAudiences::living_audience` is what
                // resolves it — while these two name the work an act was
                // performed INSIDE. Writing the one into the other was a false
                // statement at write time, and the store's reverse lookups
                // served it as fact. The audience travels separately, whole,
                // to `prepare_for_audience` below.
                program_id: None,
                engagement_id: None,
                // The reference IS the room's pointer to the exact artifact —
                // see the module note on revision enforcement.
                exact_payload_artifact_ref: document.artifact_ref.clone(),
                effective_sender: disclosed_by.to_string(),
                // Exactly one recipient per act, never empty and never the
                // whole roster: correction propagation aims at people, and an
                // act naming everyone would blur who actually saw it.
                intended_audience: vec![holder.to_string()],
                channel: OutwardChannel::Room,
                consequence_class: ConsequenceClass::ConfidentialDisclosure
                    .as_str()
                    .to_string(),
            };
            // The audience is carried, not converted: the store files the act
            // under the axis its KIND names, so an account's or a panel's room
            // is findable as itself instead of being mis-filed as an
            // engagement or dropped for having no field to sit in.
            let act = assertions.prepare_for_audience_filed(
                outward_scope,
                &request,
                &room.audience,
                &mut filed,
                now_rfc3339,
            )?;
            disclosures.push(advance_grant_effect(
                assertions,
                outward_scope,
                act,
                &grant_receipt_ref(&room.room_id, &document.artifact_ref, holder),
                now_rfc3339,
            )?);
        }
    }
    Ok(disclosures)
}

/// Advance a grant's act to the state its effect already has.
///
/// The register's activity model reads `Prepared` as *"never left"*:
/// `is_active_disclosure` excludes it, and `raise_correction_obligations`
/// skips it. Right for email, where dispatch follows preparation and can still
/// fail; wrong for a room, where the disclosure takes effect the instant the
/// grant does. An act left resting at `Prepared` here would be a live
/// disclosure the correction machinery is blind to — a corrected figure would
/// never find the holder who can still open the artifact stating it. So the
/// bridge walks each act through the store's own §4 ladder in the same call:
/// `Dispatching` (the grant is the side effect, and it is in flight the moment
/// the door opens), then `ProviderAccepted` with the grant itself as the
/// effect receipt — the room is a provider we own, and it accepted the
/// disclosure when visibility was granted. `Delivered` is deliberately not
/// claimed: acceptance is not delivery, and whether a holder actually opened
/// the document is the access log's fact (`super::access_log`); the caller may
/// `record_delivered` when a first real presentation is observed.
///
/// Idempotent across re-runs, checked against the act's **current** status: an
/// act already past `Dispatching` is returned exactly as the store holds it,
/// with nothing appended, so a re-swept room never stacks a second ladder onto
/// an advanced act. An act found at `Dispatching` is a previous run that died
/// between its two appends; the receipt append is resumed — the receipt ref is
/// derived from the same triple as the act, so the resumed receipt is the same
// Concurrency note: the status check below is read-then-append, so two
// concurrent bridge runs can both advance one act — the store's transition
// fold is last-write-wins per field and both write the same target status, so
// the race converges on ProviderAccepted with a duplicated (harmless)
// transition line. A crash between the two transitions leaves Dispatching,
// which is_active_disclosure() treats as active, so corrections still reach
// the act; the next bridge run cannot repair it (it skips acts past Prepared)
// — acceptable, and recorded here rather than discovered.
/// receipt, not a new one.
fn advance_grant_effect(
    assertions: &OutwardAssertionStore,
    outward_scope: &OutwardScope,
    act: OutwardActDisclosure,
    receipt_ref: &str,
    now_rfc3339: &str,
) -> Result<OutwardActDisclosure> {
    match act.status {
        OutwardActStatus::Prepared => {
            assertions.mark_dispatching(outward_scope, &act.outward_act_ref, now_rfc3339)?;
            assertions.record_provider_receipt(
                outward_scope,
                &act.outward_act_ref,
                receipt_ref,
                now_rfc3339,
            )?;
        },
        OutwardActStatus::Dispatching => {
            assertions.record_provider_receipt(
                outward_scope,
                &act.outward_act_ref,
                receipt_ref,
                now_rfc3339,
            )?;
        },
        _ => return Ok(act),
    }
    // Re-read rather than hand-fold: what is returned must be what the store
    // now holds, or the caller would act on a state the record does not say.
    assertions
        .load_act(outward_scope, &act.outward_act_ref)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "act `{}` unreadable immediately after its own transition was appended — the \
                 store cannot have lost a log it just wrote to",
                act.outward_act_ref
            )
        })
}

/// The effect receipt for one grant — the grant itself, named by the same
/// triple as its act.
///
/// On email the receipt ref names an outside provider's acknowledgement of the
/// send. The room has no outside provider: we own the gate, and the
/// acknowledgement IS the grant taking effect. Naming it by
/// `(room, exact reference, holder)` keeps the receipt as stable as the act it
/// settles, so a resumed advance records the same receipt rather than
/// inventing a fresh one.
fn grant_receipt_ref(room_id: &str, artifact_ref: &str, holder: &str) -> String {
    format!("room_grant{FIELD_SEP}{room_id}{FIELD_SEP}{artifact_ref}{FIELD_SEP}{holder}")
}

/// The stable key for one grant — `(room, exact document reference, holder)`.
///
/// Why each element is identity:
///
/// - **room id** — the container the grant happened through. The same document
///   shared with the same person through two different rooms is two
///   relationships disclosing, and revoking one must not erase the record of
///   the other. `DataRoomStore` derives the room id from its scope and
///   [`magician::magician_v2::audience::AudienceRef::as_key`], so the audience
///   **kind** rides in with it — rooms for `engagement:X` and `account:X` can
///   never merge — and the assertions store additionally folds its own scope
///   into the final act ref, so principal and workspace do not repeat here.
/// - **artifact ref** — the exact reference made visible. A different
///   reference is a different assertion, even inside the same room.
/// - **holder** — disclosure is per person. The same document to a second
///   holder is a second act, because correction propagation aims at people.
///
/// Stability is the dedupe: §4's *"an idempotent retry RESUMES the same
/// record, never duplicates it"* is enforced by the store deriving the act ref
/// from `(scope, key)`, so re-running the bridge over a grown room creates
/// acts only for the pairs that did not exist before. No read-before-write set
/// is kept here; the property is structural, and bookkeeping that must be kept
/// in step with reality eventually is not.
fn disclosure_idempotency_key(room_id: &str, artifact_ref: &str, holder: &str) -> String {
    format!("room_visibility{FIELD_SEP}{room_id}{FIELD_SEP}{artifact_ref}{FIELD_SEP}{holder}")
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use crate::data_room::types::{DataRoom, DocumentEntry, DocumentVisibility};
    use magician::magician_v2::agents::ConsequenceClass;
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::audience::{Audience, AudienceRef};
    use magician::magician_v2::evidence::outward_assertions::audience_axis;
    use magician::magician_v2::evidence::{
        ObligationState, OutwardActDisclosure, OutwardActStatus, OutwardAssertionStore,
        OutwardChannel, OutwardScope, PrepareOutwardAct,
    };
    use magician::magician_v2::work_context::WorkContextKind;

    use super::{disclosure_idempotency_key, record_room_disclosures};

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn now_str() -> String {
        now().to_rfc3339()
    }

    fn fixture() -> (tempfile::TempDir, OutwardAssertionStore, OutwardScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let assertions = OutwardAssertionStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, assertions, OutwardScope::new("anonymous", "default"))
    }

    fn alice() -> String {
        "alice@counterparty.test".to_string()
    }

    fn bob() -> String {
        "bob@counterparty.test".to_string()
    }

    fn engagement_audience() -> Audience {
        Audience::new(AudienceRef::engagement("eng-1"), vec![alice(), bob()])
    }

    fn doc(artifact_ref: &str) -> DocumentEntry {
        DocumentEntry {
            artifact_ref: artifact_ref.to_string(),
            visibility: DocumentVisibility::Everyone,
            added_at: now() - Duration::days(1),
            added_by: "owner".to_string(),
            withdrawn_at: None,
        }
    }

    fn open_room_for(reference: AudienceRef, documents: Vec<DocumentEntry>) -> DataRoom {
        DataRoom {
            // Unique per audience, as `DataRoomStore::derive_room_id` makes it.
            room_id: format!("room-{}", reference.as_key()),
            audience: reference,
            documents,
            opened_at: now() - Duration::days(1),
            opened_by: "owner".to_string(),
            closes_at: None,
            closed_at: None,
        }
    }

    fn open_room(documents: Vec<DocumentEntry>) -> DataRoom {
        open_room_for(AudienceRef::engagement("eng-1"), documents)
    }

    fn bridge(
        assertions: &OutwardAssertionStore,
        scope: &OutwardScope,
        room: &DataRoom,
        audience: &Audience,
        holders: &[String],
    ) -> anyhow::Result<Vec<OutwardActDisclosure>> {
        record_room_disclosures(
            assertions,
            scope,
            room,
            audience,
            holders,
            "company-assistant",
            now(),
            &now_str(),
        )
    }

    fn pairs(disclosures: &[OutwardActDisclosure]) -> Vec<(String, Vec<String>)> {
        disclosures
            .iter()
            .map(|disclosure| {
                (
                    disclosure.exact_payload_artifact_ref.clone(),
                    disclosure.intended_audience.clone(),
                )
            })
            .collect()
    }

    /// The §5 write point, per pair: *"that a claim was asserted, to these
    /// people, through this exact artifact revision, on this channel, at this
    /// time"* — one act per `(document, holder)`, the holder as the entire
    /// intended audience, on the room channel, in the confidential-disclosure
    /// class the envelopes plan assigns to a data room. And each act rests at
    /// `ProviderAccepted`, not `Prepared`: an earlier cut left room acts at
    /// `Prepared`, which `is_active_disclosure` reads as *"never left"* — a
    /// live grant invisible to correction propagation. The grant is the effect
    /// receipt; delivery stays unclaimed because acceptance is not delivery
    /// and first presentation is the access log's fact.
    #[test]
    fn every_visible_document_and_link_holder_pair_is_one_accepted_disclosure() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let room = open_room(vec![doc("artifact-a"), doc("artifact-b")]);
        let holders = vec![alice(), bob()];

        let disclosures = bridge(&assertions, &scope, &room, &audience, &holders).expect("record");

        assert_eq!(
            pairs(&disclosures),
            vec![
                ("artifact-a".to_string(), vec![alice()]),
                ("artifact-a".to_string(), vec![bob()]),
                ("artifact-b".to_string(), vec![alice()]),
                ("artifact-b".to_string(), vec![bob()]),
            ]
        );
        for disclosure in &disclosures {
            assert_eq!(disclosure.channel, OutwardChannel::Room);
            assert_eq!(disclosure.consequence_class, "confidential_disclosure");
            assert_eq!(disclosure.effective_sender, "company-assistant");
            assert_eq!(disclosure.status, OutwardActStatus::ProviderAccepted);
            assert_eq!(disclosure.prepared_at, now_str());
            assert_eq!(
                disclosure.dispatched_at.as_deref(),
                Some(now_str().as_str())
            );
            assert_eq!(
                disclosure.settled_at, None,
                "acceptance is not delivery: first presentation is the access log's fact"
            );
            assert_eq!(
                disclosure.effect_receipt_ref.as_deref(),
                Some(
                    format!(
                        "room_grant\u{1f}room-engagement:eng-1\u{1f}{}\u{1f}{}",
                        disclosure.exact_payload_artifact_ref, disclosure.intended_audience[0]
                    )
                    .as_str()
                ),
                "the effect receipt is the grant itself, named by the act's own triple"
            );
            assert_eq!(
                (
                    disclosure.engagement_id.as_deref(),
                    disclosure.program_id.as_deref()
                ),
                (None, None),
                "a room knows a relationship, not a work: `eng-1` here is the room's audience \
                 id — a counterparty id — and either field would assert it was an id it is not"
            );
            assert!(
                !disclosure.observed,
                "the room is a controlled channel: prepared before the act, never observed after"
            );
            // Advanced exactly one ladder: prepared, dispatching, accepted.
            assert_eq!(
                assertions
                    .load_act_history(&scope, &disclosure.outward_act_ref)
                    .expect("history"),
                vec![
                    OutwardActStatus::Prepared,
                    OutwardActStatus::Dispatching,
                    OutwardActStatus::ProviderAccepted,
                ]
            );
            // Durable, not just returned: the record must exist in the store.
            let held = assertions
                .load_act(&scope, &disclosure.outward_act_ref)
                .expect("load")
                .expect("a returned disclosure must be persisted");
            assert_eq!(&held, disclosure);
        }
        let distinct: std::collections::BTreeSet<&str> = disclosures
            .iter()
            .map(|disclosure| disclosure.outward_act_ref.as_str())
            .collect();
        assert_eq!(
            distinct.len(),
            4,
            "four pairs are four acts, not one blurred act"
        );
    }

    /// Deal-close §3: visibility is *"per-identity, so one room can differ per
    /// reader"* — and the disclosure record must differ with it, or it would
    /// claim we showed someone a document their visibility rule withheld.
    #[test]
    fn per_document_visibility_restricts_who_each_document_is_disclosed_to() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let mut restricted = doc("artifact-b");
        restricted.visibility = DocumentVisibility::Identities {
            identities: vec![alice()],
        };
        let room = open_room(vec![doc("artifact-a"), restricted]);

        let disclosures =
            bridge(&assertions, &scope, &room, &audience, &[alice(), bob()]).expect("record");

        assert_eq!(
            pairs(&disclosures),
            vec![
                ("artifact-a".to_string(), vec![alice()]),
                ("artifact-a".to_string(), vec![bob()]),
                ("artifact-b".to_string(), vec![alice()]),
            ]
        );
    }

    /// §4: *"an idempotent retry RESUMES the same record, never duplicates
    /// it"* — re-running the bridge after adding one document creates acts
    /// only for the new pairs, and the old pairs come back unchanged, dates
    /// and all. This must survive the grant-effect advance too: the failure
    /// pinned here is a re-run stacking a second `Dispatching` +
    /// `ProviderAccepted` ladder onto an act that already climbed it — the
    /// advance checks the current status and appends nothing to an act past
    /// `Prepared`.
    #[test]
    fn a_rerun_resumes_the_same_records_and_creates_acts_only_for_new_pairs() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let mut room = open_room(vec![doc("artifact-a")]);
        let holders = vec![alice(), bob()];

        let first = record_room_disclosures(
            &assertions,
            &scope,
            &room,
            &audience,
            &holders,
            "company-assistant",
            now(),
            &now_str(),
        )
        .expect("first");
        assert_eq!(
            pairs(&first),
            vec![
                ("artifact-a".to_string(), vec![alice()]),
                ("artifact-a".to_string(), vec![bob()]),
            ]
        );

        // The same grants seen again two hours later: the same records come
        // back, `prepared_at` untouched — resumed, not duplicated.
        let later = now() + Duration::hours(2);
        let second = record_room_disclosures(
            &assertions,
            &scope,
            &room,
            &audience,
            &holders,
            "company-assistant",
            later,
            &later.to_rfc3339(),
        )
        .expect("second");
        assert_eq!(second, first);
        for disclosure in &second {
            assert_eq!(
                assertions
                    .load_act_history(&scope, &disclosure.outward_act_ref)
                    .expect("history"),
                vec![
                    OutwardActStatus::Prepared,
                    OutwardActStatus::Dispatching,
                    OutwardActStatus::ProviderAccepted,
                ],
                "a re-run leaves an advanced act exactly as it was: no second ladder"
            );
        }

        // One document added: only the new pairs become new acts.
        room.documents.push(doc("artifact-b"));
        let third = record_room_disclosures(
            &assertions,
            &scope,
            &room,
            &audience,
            &holders,
            "company-assistant",
            later,
            &later.to_rfc3339(),
        )
        .expect("third");
        assert_eq!(third.len(), 4);
        assert_eq!(
            &third[..2],
            &first[..],
            "existing pairs keep their acts, dates and all"
        );
        assert_eq!(
            pairs(&third[2..]),
            vec![
                ("artifact-b".to_string(), vec![alice()]),
                ("artifact-b".to_string(), vec![bob()]),
            ]
        );
        assert_eq!(
            third[2].prepared_at,
            later.to_rfc3339(),
            "a new pair is prepared at the moment it became visible, never backdated"
        );
        assert_ne!(third[2].outward_act_ref, first[0].outward_act_ref);
    }

    /// The failure this pins, and the reason the bridge advances at all: room
    /// acts used to rest at `Prepared`, which `is_active_disclosure` reads as
    /// *"never left"*, so `raise_correction_obligations` skipped them — a
    /// holder kept live visibility of an artifact asserting a corrected
    /// figure, and the register concluded nobody was told. Advanced to
    /// `ProviderAccepted`, the same correction now raises exactly one open
    /// obligation aimed at that holder.
    #[test]
    fn a_correction_reaches_the_holder_of_a_room_disclosure() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let room = open_room(vec![doc("artifact-a")]);

        let disclosures =
            bridge(&assertions, &scope, &room, &audience, &[alice()]).expect("record");
        assert_eq!(disclosures.len(), 1);
        let act_ref = disclosures[0].outward_act_ref.clone();

        // The artifact the room shows asserts an approved claim to alice.
        assertions
            .record_assertion_use(
                &scope,
                &act_ref,
                "claim-q3-revenue",
                &alice(),
                &[],
                &[],
                &now_str(),
            )
            .expect("assertion use");

        // The claim is corrected. The room disclosure is a live grant, and the
        // register must owe alice a correction — zero obligations here is the
        // exact blindness this test exists to prevent.
        let obligations = assertions
            .raise_correction_obligations(&scope, "claim-q3-revenue", "correction-1", &now_str())
            .expect("raise");
        assert_eq!(
            obligations.len(),
            1,
            "a room grant is an active disclosure: the correction must find its holder"
        );
        assert_eq!(obligations[0].outward_act_ref, act_ref);
        assert_eq!(obligations[0].audience, alice());
        assert_eq!(obligations[0].approved_claim_ref, "claim-q3-revenue");
        assert_eq!(obligations[0].correction_ref, "correction-1");
        assert_eq!(
            obligations[0].disclosure_status_at_raise,
            OutwardActStatus::ProviderAccepted
        );
        assert_eq!(obligations[0].state, ObligationState::Open);
    }

    /// The advance's own crash window: a run that died between `Dispatching`
    /// and the receipt leaves the act mid-ladder, and the failure pinned here
    /// is a re-run either erroring on it or restarting the ladder with a
    /// second `Dispatching`. The re-run must RESUME at the receipt — same act,
    /// same receipt, one ladder.
    #[test]
    fn a_run_that_died_mid_advance_is_resumed_at_the_receipt() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let room = open_room(vec![doc("artifact-a")]);

        // Reproduce the window by hand: the act prepared under the bridge's
        // own idempotency key and marked dispatching, then the process died
        // before the receipt could be appended.
        let request = PrepareOutwardAct {
            idempotency_key: disclosure_idempotency_key(&room.room_id, "artifact-a", &alice()),
            // Exactly what the bridge writes: a room names a relationship, not
            // a work, so neither field is filled and the audience is carried
            // whole on the call below.
            program_id: None,
            engagement_id: None,
            exact_payload_artifact_ref: "artifact-a".to_string(),
            effective_sender: "company-assistant".to_string(),
            intended_audience: vec![alice()],
            channel: OutwardChannel::Room,
            consequence_class: ConsequenceClass::ConfidentialDisclosure
                .as_str()
                .to_string(),
        };
        let stuck = assertions
            .prepare_for_audience(&scope, &request, &room.audience, &now_str())
            .expect("prepare");
        assertions
            .mark_dispatching(&scope, &stuck.outward_act_ref, &now_str())
            .expect("dispatching");

        let disclosures =
            bridge(&assertions, &scope, &room, &audience, &[alice()]).expect("resume");
        assert_eq!(disclosures.len(), 1);
        assert_eq!(disclosures[0].outward_act_ref, stuck.outward_act_ref);
        assert_eq!(disclosures[0].status, OutwardActStatus::ProviderAccepted);
        assert_eq!(
            disclosures[0].effect_receipt_ref.as_deref(),
            Some(
                format!(
                    "room_grant\u{1f}{}\u{1f}artifact-a\u{1f}{}",
                    room.room_id,
                    alice()
                )
                .as_str()
            )
        );
        assert_eq!(
            assertions
                .load_act_history(&scope, &stuck.outward_act_ref)
                .expect("history"),
            vec![
                OutwardActStatus::Prepared,
                OutwardActStatus::Dispatching,
                OutwardActStatus::ProviderAccepted,
            ],
            "resumed at the receipt: one `Dispatching`, never two"
        );
    }

    /// A withdrawn entry is kept for audit but is not present — and what is
    /// not present is not visible, so nothing is disclosed through it.
    #[test]
    fn a_withdrawn_document_is_not_disclosed() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let mut withdrawn = doc("artifact-b");
        withdrawn.withdrawn_at = Some(now() - Duration::hours(3));
        let room = open_room(vec![doc("artifact-a"), withdrawn]);

        let disclosures =
            bridge(&assertions, &scope, &room, &audience, &[alice()]).expect("record");

        assert_eq!(
            pairs(&disclosures),
            vec![("artifact-a".to_string(), vec![alice()])]
        );
    }

    /// A room that is not open shows nobody anything. The write point must
    /// agree with `visible_to`, or the register would carry disclosures the
    /// room never made.
    #[test]
    fn a_room_that_is_not_open_discloses_nothing() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();

        let mut closed = open_room(vec![doc("artifact-a")]);
        closed.closed_at = Some(now() - Duration::hours(1));
        assert_eq!(
            bridge(&assertions, &scope, &closed, &audience, &[alice()]).expect("closed"),
            Vec::new()
        );

        let mut expired = open_room(vec![doc("artifact-a")]);
        // Inclusive, like every expiry in this codebase: closing at noon means
        // closed at noon.
        expired.closes_at = Some(now());
        assert_eq!(
            bridge(&assertions, &scope, &expired, &audience, &[alice()]).expect("expired"),
            Vec::new()
        );
    }

    /// A wrong roster is a caller bug, not an absence: silence would attribute
    /// confidential disclosures to the wrong relationship. Same id with a
    /// different kind must refuse too — `engagement:X` and `account:X` are
    /// different relationships, the exact merge `as_key` exists to prevent.
    #[test]
    fn a_roster_for_a_different_relationship_is_an_error_not_an_absence() {
        let (_tmp, assertions, scope) = fixture();
        let room = open_room(vec![doc("artifact-a")]);

        let wrong_kind = Audience::new(AudienceRef::account("eng-1"), vec![alice()]);
        let err = bridge(&assertions, &scope, &room, &wrong_kind, &[alice()])
            .expect_err("a kind mismatch must refuse, not disclose");
        let message = err.to_string();
        assert!(message.contains("account:eng-1"), "{message}");
        assert!(message.contains("engagement:eng-1"), "{message}");

        let wrong_id = Audience::new(AudienceRef::engagement("eng-2"), vec![alice()]);
        assert!(bridge(&assertions, &scope, &room, &wrong_id, &[alice()]).is_err());
    }

    /// A room's acts are filed under its audience's own axis, and never under
    /// a work axis.
    ///
    /// The failure this pins is the one shipped here first: a room's audience
    /// id is a **counterparty** id — `CounterpartyAudiences::living_audience`
    /// resolves it through the counterparty register, so a room cannot open on
    /// anything else — and it was being copied into `engagement_id`, the field
    /// that names the engagement an act was performed inside. Every such act
    /// asserted an id it did not have, and the engagement axis served the
    /// assertion as fact. The mirror failure is `Account`, `Panel` and
    /// `Person`: no work field could hold them, so their grants were filed
    /// under no relationship at all and a sweep for a panel's disclosures found
    /// nothing — indistinguishable from a panel that was shown nothing.
    #[test]
    fn a_rooms_acts_file_under_its_audiences_axis_never_a_work_axis() {
        let (_tmp, assertions, scope) = fixture();
        for reference in [
            AudienceRef::engagement("counterparty-acme"),
            AudienceRef::program("q3-intake"),
            AudienceRef::account("acme-group"),
            AudienceRef::panel("audit-2026"),
            AudienceRef::person("dana@counterparty.test"),
        ] {
            let audience = Audience::new(reference.clone(), vec![alice()]);
            let room = open_room_for(reference.clone(), vec![doc("artifact-a")]);
            let disclosures =
                bridge(&assertions, &scope, &room, &audience, &[alice()]).expect("record");
            assert_eq!(disclosures.len(), 1, "{}", reference.as_key());
            assert_eq!(
                (
                    disclosures[0].engagement_id.as_deref(),
                    disclosures[0].program_id.as_deref()
                ),
                (None, None),
                "{} — a room knows no work, and either field would assert one",
                reference.as_key()
            );
            assert!(
                assertions
                    .index_entries(&scope, audience_axis(reference.kind), &reference.id)
                    .expect("audience axis")
                    .contains(&disclosures[0].outward_act_ref),
                "{} — a grant nothing can find by its own relationship is a disclosure the \
                 correction machinery cannot reach",
                reference.as_key()
            );
        }

        // The other half of the same fact, asked from the work side: no room
        // grant may be reachable as work. Asked over the whole scope, so an act
        // filed under some third value would fail this too.
        for axis in WorkContextKind::KIND_TOKENS {
            assert!(
                assertions
                    .act_refs_under_axis(&scope, axis)
                    .expect("work axis listing")
                    .is_empty(),
                "the `{axis}` work axis holds a room grant: an act performed for a counterparty \
                 is being served as an act performed inside a work of that id"
            );
        }
    }

    /// Access derives from the audience: a link holder the audience does not
    /// admit sees nothing, so nothing is disclosed to them. A stale or wrong
    /// link is inert here, exactly as a stranger on a per-document allow-list
    /// is inert in `permits`.
    #[test]
    fn a_link_holder_the_audience_does_not_admit_is_excluded() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let room = open_room(vec![doc("artifact-a")]);
        let holders = vec![alice(), "mallory@elsewhere.test".to_string()];

        let disclosures = bridge(&assertions, &scope, &room, &audience, &holders).expect("record");

        assert_eq!(
            pairs(&disclosures),
            vec![("artifact-a".to_string(), vec![alice()])]
        );
    }

    /// Membership includes the relationship still being current — *"treating
    /// 'was listed' as 'may see' is how access outlives the relationship it
    /// came from"* (the audience module's invariant). An ended relationship is
    /// disclosed nothing, live link or not.
    #[test]
    fn an_ended_relationship_is_disclosed_nothing_even_with_a_live_link() {
        let (_tmp, assertions, scope) = fixture();
        // Inclusive: expiring at noon means expired at noon.
        let audience = engagement_audience().expiring_at(now());
        let room = open_room(vec![doc("artifact-a")]);

        assert_eq!(
            bridge(&assertions, &scope, &room, &audience, &[alice()]).expect("record"),
            Vec::new()
        );
    }

    /// One grant is one record: a holder repeated in the caller's list must
    /// not appear twice in the result, and the store must hold one act.
    #[test]
    fn a_duplicated_link_holder_is_one_grant_and_one_record() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let room = open_room(vec![doc("artifact-a")]);

        let disclosures =
            bridge(&assertions, &scope, &room, &audience, &[alice(), alice()]).expect("record");

        assert_eq!(
            pairs(&disclosures),
            vec![("artifact-a".to_string(), vec![alice()])]
        );
    }

    /// A room nobody can reach yet has disclosed nothing: an empty holder list
    /// is an absence, not a failure — and because every act names exactly one
    /// recipient, no act with an empty `intended_audience` can ever exist to
    /// vacuously satisfy a recipient predicate downstream.
    #[test]
    fn no_live_links_means_nothing_disclosed_not_an_error() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();
        let room = open_room(vec![doc("artifact-a")]);

        assert_eq!(
            bridge(&assertions, &scope, &room, &audience, &[]).expect("no holders"),
            Vec::new()
        );
    }

    /// Fail closed on inputs that would corrupt the record: a blank room id
    /// would merge grants from different rooms into one act, and a blank
    /// sender would record an assertion nobody made.
    #[test]
    fn an_unnamed_room_or_sender_cannot_disclose() {
        let (_tmp, assertions, scope) = fixture();
        let audience = engagement_audience();

        let mut unnamed = open_room(vec![doc("artifact-a")]);
        unnamed.room_id = "   ".to_string();
        assert!(bridge(&assertions, &scope, &unnamed, &audience, &[alice()]).is_err());

        let room = open_room(vec![doc("artifact-a")]);
        let err = record_room_disclosures(
            &assertions,
            &scope,
            &room,
            &audience,
            &[alice()],
            "   ",
            now(),
            &now_str(),
        )
        .expect_err("a blank sender must refuse");
        assert!(err.to_string().contains("who granted"), "{err}");
    }
}
