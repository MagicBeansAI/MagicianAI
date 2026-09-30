//! Phase 1's properties, as behaviour.

use chrono::{Duration, TimeZone, Utc};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{Audience, AudienceKind, AudienceRef};
use magician::magician_v2::evidence::{
    OutwardActStatus, OutwardAssertionStore, OutwardChannel, OutwardScope,
};

use super::store::{DataRoomScope, DataRoomStore, GrantDisclosure};
use super::types::{DataRoom, DocumentVisibility, OpenDataRoom, RoomStanding};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn store() -> (tempfile::TempDir, DataRoomStore, DataRoomScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = DataRoomStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, DataRoomScope::new("anonymous", "default"))
}

/// The outward-assertions register the room accounts to, rooted in the same
/// workspace as the room itself — which is where the store derives the outward
/// scope from, so a test reads back through the same path production does.
fn register(tmp: &tempfile::TempDir) -> OutwardAssertionStore {
    OutwardAssertionStore::new(ArtifactV2Workspace::new(tmp.path()))
}

/// The scope the store derives for the register, spelled out so reads in the
/// tests cannot silently diverge from the writes the store makes.
fn outward_scope(scope: &DataRoomScope) -> OutwardScope {
    OutwardScope::new(scope.principal.clone(), scope.workspace.clone())
}

/// Grant a document with a named set of live link holders.
fn grant_to(
    store: &DataRoomStore,
    scope: &DataRoomScope,
    assertions: &OutwardAssertionStore,
    room_id: &str,
    artifact_ref: &str,
    visibility: DocumentVisibility,
    holders: &[String],
    at: chrono::DateTime<Utc>,
) -> anyhow::Result<DataRoom> {
    store.add_document(
        scope,
        room_id,
        artifact_ref,
        visibility,
        "owner",
        &GrantDisclosure {
            assertions,
            audience: &audience(),
            holders,
            disclosed_by: "owner",
        },
        at,
    )
}

/// Grant a document with **no** live link holders.
///
/// The honest absence, and what every test that is not about disclosure wants:
/// nothing can be reached yet, so nothing is disclosed — but the grant still
/// has to be able to account for itself, which is why the register and the
/// roster are handed in regardless.
fn add(
    store: &DataRoomStore,
    scope: &DataRoomScope,
    assertions: &OutwardAssertionStore,
    room_id: &str,
    artifact_ref: &str,
    at: chrono::DateTime<Utc>,
) -> anyhow::Result<DataRoom> {
    grant_to(
        store,
        scope,
        assertions,
        room_id,
        artifact_ref,
        DocumentVisibility::Everyone,
        &[],
        at,
    )
}

fn opened(
    store: &DataRoomStore,
    scope: &DataRoomScope,
    closes_at: Option<chrono::DateTime<Utc>>,
) -> String {
    store
        .open(
            scope,
            &OpenDataRoom {
                audience: AudienceRef::engagement("eng-1"),
                opened_by: "owner".to_string(),
                closes_at,
            },
            now(),
        )
        .expect("open")
        .room_id
}

fn roster() -> Vec<String> {
    vec![
        "alice@example.com".to_string(),
        "bob@example.com".to_string(),
    ]
}

/// The audience a room is opened for, with everyone in it and no end date.
fn audience() -> Audience {
    Audience::new(AudienceRef::engagement("eng-1"), roster())
}

/// One engagement, one room. Two rooms for one counterparty would mean two
/// answers to "what have they seen", and the wrong one would be whichever a
/// caller happened to reach.
#[test]
fn one_engagement_has_exactly_one_room() {
    let (_tmp, store, scope) = store();
    let first = opened(&store, &scope, None);
    let again = opened(&store, &scope, None);
    assert_eq!(first, again, "re-opening returns the same room");

    let found = store
        .for_audience(&scope, &AudienceRef::engagement("eng-1"))
        .expect("lookup")
        .expect("the room exists");
    assert_eq!(found.room_id, first);
    assert!(store
        .for_audience(&scope, &AudienceRef::engagement("eng-2"))
        .expect("lookup")
        .is_none());

    // The KIND is part of the identity: the same company as a live deal and as a
    // standing client are different audiences, so they get different rooms.
    assert!(store
        .for_audience(&scope, &AudienceRef::account("eng-1"))
        .expect("lookup")
        .is_none());
}

/// The generalisation, as behaviour: the same container serves every kind of
/// relationship, and nothing about the room changes between them.
#[test]
fn a_room_serves_any_kind_of_audience() {
    let (_tmp, store, scope) = store();
    let mut rooms = Vec::new();

    for reference in [
        AudienceRef::engagement("acme"),
        AudienceRef::program("q3-intake"),
        AudienceRef::account("acme"),
        AudienceRef::panel("audit-2026"),
        AudienceRef::person("new-hire-14"),
    ] {
        let room = store
            .open(
                &scope,
                &OpenDataRoom {
                    audience: reference.clone(),
                    opened_by: "owner".to_string(),
                    closes_at: None,
                },
                now(),
            )
            .expect("open");
        assert_eq!(room.audience, reference);
        rooms.push(room.room_id);
    }

    rooms.sort();
    rooms.dedup();
    assert_eq!(rooms.len(), 5, "five audiences, five distinct rooms");
}

/// A room without an engagement has nothing for access to derive from.
#[test]
fn a_room_must_belong_to_an_audience() {
    let (_tmp, store, scope) = store();
    assert!(store
        .open(
            &scope,
            &OpenDataRoom {
                audience: AudienceRef::new(AudienceKind::Engagement, "  "),
                opened_by: "owner".to_string(),
                closes_at: None,
            },
            now(),
        )
        .is_err());
}

/// Add, list, withdraw — and the withdrawn entry is KEPT, because "this was in
/// the room between March and April" is the question an audit asks.
#[test]
fn a_withdrawn_document_leaves_the_room_but_not_the_record() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);

    add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("add");
    let room = add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://financials@1",
        now(),
    )
    .expect("add");
    assert_eq!(room.present_documents().len(), 2);

    let room = store
        .withdraw_document(
            &scope,
            &room_id,
            "artifact://financials@1",
            now() + Duration::days(1),
        )
        .expect("withdraw");
    assert_eq!(room.present_documents().len(), 1);
    assert_eq!(
        room.documents.len(),
        2,
        "the withdrawn entry is kept — a deleted row cannot say it was ever there"
    );
    let withdrawn = room
        .documents
        .iter()
        .find(|entry| entry.artifact_ref == "artifact://financials@1")
        .expect("still recorded");
    assert_eq!(withdrawn.withdrawn_at, Some(now() + Duration::days(1)));
    assert!(!withdrawn.is_present());
}

/// Adding the same document twice does not list it twice; re-adding a withdrawn
/// one restores it, which is what "add" plainly means.
#[test]
fn adding_is_idempotent_and_re_adding_restores() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);

    add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("add");
    let room = add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("add again");
    assert_eq!(room.present_documents().len(), 1);

    store
        .withdraw_document(&scope, &room_id, "artifact://deck@1", now())
        .expect("withdraw");
    let room = add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now() + Duration::hours(1),
    )
    .expect("re-add");
    assert_eq!(
        room.present_documents().len(),
        1,
        "restored, not duplicated"
    );
    assert_eq!(room.documents.len(), 1);
}

/// Withdrawing something that is not there is a no-op, not an error: a surface
/// double-clicking must not produce an error to interpret.
#[test]
fn withdrawing_an_absent_document_is_a_no_op() {
    let (_tmp, store, scope) = store();
    let room_id = opened(&store, &scope, None);
    let room = store
        .withdraw_document(&scope, &room_id, "artifact://never-added@1", now())
        .expect("no-op");
    assert!(room.documents.is_empty());
}

/// Standing is derived, never stored — a stored status would drift the moment a
/// room expires with nobody writing to it.
#[test]
fn standing_is_derived_from_the_engagements_clock() {
    let (_tmp, store, scope) = store();
    let room_id = opened(&store, &scope, Some(now() + Duration::days(7)));
    let room = store.load(&scope, &room_id).expect("load").expect("room");

    assert_eq!(room.standing(now()), RoomStanding::Open);
    assert_eq!(
        room.standing(now() + Duration::days(8)),
        RoomStanding::Expired,
        "the room closes on the engagement's clock with nobody writing to it"
    );

    // Expiry is inclusive.
    assert_eq!(
        room.standing(now() + Duration::days(7)),
        RoomStanding::Expired
    );

    let closed = store
        .close(&scope, &room_id, "owner", now())
        .expect("close");
    assert_eq!(closed.standing(now()), RoomStanding::Closed);
    assert!(!RoomStanding::Closed.is_open());
    assert!(!RoomStanding::Expired.is_open());
    assert!(RoomStanding::Open.is_open());
}

/// A closed room takes nothing. Adding to one would make it look, later, as
/// though a document was available when it was not.
#[test]
fn a_closed_or_expired_room_takes_no_documents() {
    // Both fixtures are created up front: `store` the local binding shadows
    // `store()` the helper for the rest of this function.
    let (_tmp, closed_store, closed_scope) = store();
    let (_tmp2, store2, scope2) = store();
    let closed_assertions = register(&_tmp);
    let assertions2 = register(&_tmp2);

    let room_id = opened(
        &closed_store,
        &closed_scope,
        Some(now() + Duration::days(1)),
    );
    closed_store
        .close(&closed_scope, &room_id, "owner", now())
        .expect("close");
    assert!(add(
        &closed_store,
        &closed_scope,
        &closed_assertions,
        &room_id,
        "artifact://late@1",
        now()
    )
    .is_err());

    // And an expired room refuses on the clock alone, with nobody closing it.
    let expiring = opened(&store2, &scope2, Some(now() + Duration::days(1)));
    assert!(add(
        &store2,
        &scope2,
        &assertions2,
        &expiring,
        "artifact://late@1",
        now() + Duration::days(2)
    )
    .is_err());
}

/// Closing keeps every entry — what was in the room is exactly what an owner
/// wants after closing it. And closing twice is idempotent.
#[test]
fn closing_preserves_the_contents_and_is_idempotent() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);
    add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("add");

    let closed = store
        .close(&scope, &room_id, "owner", now())
        .expect("close");
    assert_eq!(closed.documents.len(), 1);
    assert_eq!(closed.closed_at, Some(now()));

    let again = store
        .close(&scope, &room_id, "owner", now() + Duration::days(1))
        .expect("close twice");
    assert_eq!(again.closed_at, Some(now()), "the first close is the close");
}

/// Access derives from the ENGAGEMENT. A per-document allow-list must not
/// outlive the relationship it belongs to.
#[test]
fn a_reader_removed_from_the_audience_sees_nothing() {
    let everyone = DocumentVisibility::Everyone;
    let named = DocumentVisibility::Identities {
        identities: vec!["alice@example.com".to_string()],
    };

    assert!(everyone.permits("alice@example.com", &audience(), now()));
    assert!(named.permits("alice@example.com", &audience(), now()));
    assert!(
        !named.permits("bob@example.com", &audience(), now()),
        "one room can differ per reader"
    );

    // Removed from the audience — both forms refuse, including the one that
    // names them.
    let shrunk = Audience::new(
        AudienceRef::engagement("eng-1"),
        vec!["bob@example.com".to_string()],
    );
    assert!(!everyone.permits("alice@example.com", &shrunk, now()));
    assert!(
        !named.permits("alice@example.com", &shrunk, now()),
        "a per-document allow-list must not outlive the audience"
    );

    // And an audience whose relationship has ENDED admits nobody, even someone
    // still on the list. Access must not outlive the relationship it came from.
    let ended = audience().expiring_at(now() - Duration::days(1));
    assert!(!everyone.permits("alice@example.com", &ended, now()));
    assert!(!named.permits("alice@example.com", &ended, now()));
}

/// `Everyone` means every identity on the engagement — never the public. A
/// stranger is not a reader at all.
#[test]
fn everyone_means_the_audience_not_the_public() {
    assert!(!DocumentVisibility::Everyone.permits("stranger@example.com", &audience(), now()));
}

/// A closed room shows nobody anything, whatever the per-document rules say.
/// Evaluating document rules first would let an allow-list read as access.
#[test]
fn a_closed_room_shows_nobody_anything() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);
    add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("add");

    let open = store.load(&scope, &room_id).expect("load").expect("room");
    assert_eq!(
        open.visible_to("alice@example.com", &audience(), now())
            .len(),
        1
    );

    let closed = store
        .close(&scope, &room_id, "owner", now())
        .expect("close");
    assert!(
        closed
            .visible_to("alice@example.com", &audience(), now())
            .is_empty(),
        "a closed room shows nothing even to someone the document names"
    );
}

/// Two clocks, and both must be running. A room left open past the end of the
/// audience it serves would be exactly the "access outlives the relationship"
/// failure the model exists to prevent.
#[test]
fn an_open_room_shows_nothing_once_its_audience_has_ended() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);
    add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("add");
    let room = store.load(&scope, &room_id).expect("load").expect("room");

    // The room itself never closes.
    assert!(room.standing(now() + Duration::days(365)).is_open());

    let ended = audience().expiring_at(now() + Duration::days(30));
    assert_eq!(
        room.visible_to("alice@example.com", &ended, now()).len(),
        1,
        "while the relationship runs"
    );
    assert!(
        room.visible_to("alice@example.com", &ended, now() + Duration::days(31))
            .is_empty(),
        "the relationship ended, so the room shows nothing — whatever its own clock says"
    );
}

/// A roster handed in from somewhere else must not open a room it was never
/// opened for.
#[test]
fn a_room_refuses_an_audience_it_was_not_opened_for() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);
    add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("add");
    let room = store.load(&scope, &room_id).expect("load").expect("room");

    let someone_elses = Audience::new(AudienceRef::engagement("eng-2"), roster());
    assert!(
        room.visible_to("alice@example.com", &someone_elses, now())
            .is_empty(),
        "the identities match, but this is not this room's audience"
    );

    // Same id, different KIND — still not this room's audience.
    let wrong_kind = Audience::new(AudienceRef::account("eng-1"), roster());
    assert!(room
        .visible_to("alice@example.com", &wrong_kind, now())
        .is_empty());
}

/// Scopes do not leak into each other.
#[test]
fn one_scopes_rooms_are_not_anothers() {
    let (_tmp, store, scope) = store();
    opened(&store, &scope, None);
    let other = DataRoomScope::new("someone-else", "default");
    assert!(store
        .for_audience(&other, &AudienceRef::engagement("eng-1"))
        .expect("lookup")
        .is_none());
}

// ── The grant IS a disclosure (phase 3's write point, wired) ────────────────

/// The failure this pins: `record_room_disclosures` had no production caller,
/// so a document could become visible to a link holder with nothing in the
/// register saying so. One grant of two documents to two holders is four acts
/// — one per `(document, holder)` pair, each naming exactly one recipient on
/// the room channel — and the acts are read back out of the register, not out
/// of a return value the store could have fabricated.
#[test]
fn a_grant_records_exactly_one_disclosure_per_document_and_link_holder() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let room_id = opened(&store, &scope, None);
    let holders = roster();

    for artifact_ref in ["artifact://deck@1", "artifact://financials@1"] {
        grant_to(
            &store,
            &scope,
            &assertions,
            &room_id,
            artifact_ref,
            DocumentVisibility::Everyone,
            &holders,
            now(),
        )
        .expect("grant");
    }

    let mut pairs: Vec<(String, String)> = Vec::new();
    for artifact_ref in ["artifact://deck@1", "artifact://financials@1"] {
        let act_refs = assertions
            .index_entries(&outward, "artifact", artifact_ref)
            .expect("artifact index");
        assert_eq!(
            act_refs.len(),
            2,
            "{artifact_ref}: one act per link holder, never one act for the room"
        );
        for act_ref in act_refs {
            let act = assertions
                .load_act(&outward, &act_ref)
                .expect("load")
                .expect("a granted disclosure must be persisted");
            assert_eq!(act.channel, OutwardChannel::Room);
            assert_eq!(act.consequence_class, "confidential_disclosure");
            assert_eq!(act.effective_sender, "owner");
            // Both work fields are `None`, and that is the fix rather than a
            // regression. A room's `audience.id` is a COUNTERPARTY id — the
            // audience resolves through `counterparty_store.load` — so writing
            // it into `engagement_id` filed every room-borne act under a false
            // statement of fact, and `reindex_work_axes` then counted those
            // acts as correctly attributed. The act is filed by its AUDIENCE
            // now; the work fields say nothing rather than something untrue.
            assert_eq!(act.engagement_id, None);
            assert_eq!(act.program_id, None);
            assert_eq!(
                act.audience,
                Some(AudienceRef::engagement("eng-1")),
                "the act must still say WHO it was for — dropping the false work \
                 id must not also drop the relationship"
            );
            assert_eq!(act.exact_payload_artifact_ref, artifact_ref);
            assert_eq!(
                act.status,
                OutwardActStatus::ProviderAccepted,
                "the grant IS the effect: an act resting at Prepared is a live grant \
                 correction propagation cannot see"
            );
            assert_eq!(
                act.intended_audience.len(),
                1,
                "correction propagation aims at people, so an act names one holder"
            );
            pairs.push((artifact_ref.to_string(), act.intended_audience[0].clone()));
        }
    }

    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            (
                "artifact://deck@1".to_string(),
                "alice@example.com".to_string()
            ),
            (
                "artifact://deck@1".to_string(),
                "bob@example.com".to_string()
            ),
            (
                "artifact://financials@1".to_string(),
                "alice@example.com".to_string()
            ),
            (
                "artifact://financials@1".to_string(),
                "bob@example.com".to_string()
            ),
        ]
    );
}

/// Record-before-grant, as behaviour: a grant whose disclosure cannot be
/// recorded leaves NO grant. The room is read back from its own log, because
/// the failure being pinned is a document that became visible while its record
/// failed — a disclosure nobody can account for, and one an error return alone
/// would not catch.
#[test]
fn a_grant_whose_disclosure_cannot_be_recorded_leaves_no_grant() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let room_id = opened(&store, &scope, None);
    let holders = roster();

    // A roster for a different relationship: the bridge refuses it rather than
    // recording against the wrong counterparty.
    let wrong_relationship = Audience::new(AudienceRef::account("eng-1"), roster());
    let refused = store.add_document(
        &scope,
        &room_id,
        "artifact://financials@1",
        DocumentVisibility::Everyone,
        "owner",
        &GrantDisclosure {
            assertions: &assertions,
            audience: &wrong_relationship,
            holders: &holders,
            disclosed_by: "owner",
        },
        now(),
    );
    assert!(refused.is_err());

    let room = store.load(&scope, &room_id).expect("load").expect("room");
    assert!(
        room.documents.is_empty(),
        "the grant failed closed: no entry, present or withdrawn, in the room's log"
    );
    assert!(room
        .visible_to("alice@example.com", &audience(), now())
        .is_empty());
    assert_eq!(
        assertions
            .index_entries(&outward, "artifact", "artifact://financials@1")
            .expect("artifact index"),
        Vec::<String>::new(),
        "and nothing half-recorded either"
    );

    // A blank discloser is refused the same way: `effective_sender` is how the
    // register answers *who told them*.
    assert!(store
        .add_document(
            &scope,
            &room_id,
            "artifact://financials@1",
            DocumentVisibility::Everyone,
            "owner",
            &GrantDisclosure {
                assertions: &assertions,
                audience: &audience(),
                holders: &holders,
                disclosed_by: "   ",
            },
            now(),
        )
        .is_err());
    assert!(store
        .load(&scope, &room_id)
        .expect("load")
        .expect("room")
        .documents
        .is_empty());
}

/// Per-document visibility decides who a grant discloses to, and the record
/// must agree with it — otherwise the register would claim we showed someone a
/// document their visibility rule withheld. A room with no live links records
/// nothing at all, which is an absence rather than a failure: the document is
/// still granted.
#[test]
fn a_grant_discloses_only_to_the_holders_its_visibility_admits() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let room_id = opened(&store, &scope, None);

    let room = grant_to(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://cap-table@1",
        DocumentVisibility::Identities {
            identities: vec!["alice@example.com".to_string()],
        },
        &roster(),
        now(),
    )
    .expect("grant");
    assert_eq!(room.present_documents().len(), 1);

    let act_refs = assertions
        .index_entries(&outward, "artifact", "artifact://cap-table@1")
        .expect("artifact index");
    assert_eq!(
        act_refs.len(),
        1,
        "bob is a link holder the document withholds"
    );
    let act = assertions
        .load_act(&outward, &act_refs[0])
        .expect("load")
        .expect("act");
    assert_eq!(act.intended_audience, vec!["alice@example.com".to_string()]);

    // No live link at all: the grant still lands, and discloses to nobody.
    let room = add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        now(),
    )
    .expect("grant with no holders");
    assert_eq!(room.present_documents().len(), 2);
    assert_eq!(
        assertions
            .index_entries(&outward, "artifact", "artifact://deck@1")
            .expect("artifact index"),
        Vec::<String>::new(),
        "no live link, no grant to account for — and no act with an empty audience \
         to satisfy a recipient lookup vacuously"
    );
}

/// The separator claim, enforced at the grant rather than assumed: the
/// disclosure's idempotency key is `(room, artifact ref, holder)` joined by
/// U+001F, so a component carrying it could shift bytes across the separator
/// and fuse two documents' — or two people's — disclosures into one record.
/// The grant refuses, and nothing lands in the room.
#[test]
fn a_reference_or_holder_carrying_the_separator_cannot_be_granted() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);

    assert!(add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck\u{1f}artifact://financials",
        now()
    )
    .is_err());

    assert!(grant_to(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        DocumentVisibility::Everyone,
        &["alice@example.com\u{1f}bob@example.com".to_string()],
        now(),
    )
    .is_err());

    assert!(
        store
            .load(&scope, &room_id)
            .expect("load")
            .expect("room")
            .documents
            .is_empty(),
        "a refused grant leaves the room untouched"
    );
}

/// A re-grant RESUMES the same records rather than stacking second ones: the
/// act's identity is `(room, artifact ref, holder)`, so re-adding a document
/// that is already present — the idempotent path — records against the room
/// unchanged and appends nothing new. The failure pinned is a re-run doubling
/// the register's answer to "how many times did we disclose this".
#[test]
fn re_granting_a_present_document_resumes_its_records() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let room_id = opened(&store, &scope, None);
    let holders = roster();

    grant_to(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        DocumentVisibility::Everyone,
        &holders,
        now(),
    )
    .expect("grant");
    let again = grant_to(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck@1",
        DocumentVisibility::Everyone,
        &holders,
        now() + Duration::hours(2),
    )
    .expect("re-grant");
    assert_eq!(again.documents.len(), 1, "one entry, not two");

    let act_refs = assertions
        .index_entries(&outward, "artifact", "artifact://deck@1")
        .expect("artifact index");
    assert_eq!(act_refs.len(), 2, "two holders, two acts — still");
    for act_ref in &act_refs {
        assert_eq!(
            assertions
                .load_act_history(&outward, act_ref)
                .expect("history"),
            vec![
                OutwardActStatus::Prepared,
                OutwardActStatus::Dispatching,
                OutwardActStatus::ProviderAccepted,
            ],
            "a re-grant climbs no second ladder"
        );
        assert_eq!(
            assertions
                .load_act(&outward, act_ref)
                .expect("load")
                .expect("act")
                .prepared_at,
            now().to_rfc3339(),
            "the disclosure happened when the door opened, and is never backdated \
             or re-dated by a re-grant"
        );
    }
}

// ── The audit log (phase 3) ─────────────────────────────────────────────────

use super::access_log::{
    attention_across, attention_for, document_reach, AccessEvent, AttentionSignal, UserAgentClass,
    ROOM_LOGGING_NOTICE,
};

fn access(
    token: &str,
    document: Option<&str>,
    at: chrono::DateTime<Utc>,
    sequence: u32,
) -> AccessEvent {
    AccessEvent {
        room_id: "room-1".to_string(),
        audience: AudienceRef::engagement("eng-1"),
        token_issued_to: token.to_string(),
        document_ref: document.map(str::to_string),
        occurred_at: at,
        dwell_ms: None,
        sequence,
        user_agent_class: UserAgentClass::Unknown,
    }
}

fn documents() -> Vec<String> {
    vec![
        "artifact://deck@1".to_string(),
        "artifact://financials@1".to_string(),
    ]
}

/// §6's opening sentence, as behaviour: *"opened the deck three times, never
/// opened the financials."*
#[test]
fn the_log_says_what_was_reached_and_what_was_not() {
    let events = vec![
        access("alice@example.com", Some("artifact://deck@1"), now(), 1),
        access(
            "alice@example.com",
            Some("artifact://deck@1"),
            now() + Duration::hours(2),
            2,
        ),
        access(
            "alice@example.com",
            Some("artifact://deck@1"),
            now() + Duration::days(1),
            3,
        ),
    ];

    let attention = attention_for("alice@example.com", &documents(), &events);
    assert_eq!(attention.visits, 3);
    assert_eq!(attention.presentations, 3);
    assert_eq!(attention.signal(), AttentionSignal::OpenedRepeatedly);
    assert_eq!(
        attention.documents_opened,
        vec!["artifact://deck@1".to_string()]
    );
    assert_eq!(
        attention.documents_unopened,
        vec!["artifact://financials@1".to_string()],
        "what the next conversation is about"
    );
    assert!(attention.is_partial());

    assert_eq!(document_reach(&events).get("artifact://deck@1"), Some(&3));
    assert!(document_reach(&events)
        .get("artifact://financials@1")
        .is_none());
}

/// The signal that earns the feature. A token that never appears in the log is
/// the case — so the shared-with list must be supplied, not derived from events,
/// or the most important signal would be invisible.
#[test]
fn never_opened_is_visible_only_because_the_share_list_is_supplied() {
    let events = vec![access(
        "alice@example.com",
        Some("artifact://deck@1"),
        now(),
        1,
    )];
    let shared = vec![
        "alice@example.com".to_string(),
        "bob@example.com".to_string(),
    ];

    let all = attention_across(&shared, &documents(), &events);
    assert_eq!(all.len(), 2, "both tokens appear, including the silent one");

    let bob = all
        .iter()
        .find(|a| a.token_issued_to == "bob@example.com")
        .expect("the token that never came back");
    assert_eq!(bob.signal(), AttentionSignal::NeverOpened);
    assert_eq!(bob.presentations, 0);
    assert!(
        bob.signal().is_delivery_question(),
        "never-opened is a delivery question, not a nudge — pestering someone who \
         never received the mail is the wrong action"
    );

    // Never-opened sorts first: it is the one worth acting on.
    assert_eq!(all[0].token_issued_to, "bob@example.com");
}

/// "They never came" is not a partial read. Conflating them would turn the
/// strongest delivery signal in the set into a content observation.
#[test]
fn never_opened_is_not_partial() {
    let attention = attention_for("ghost@example.com", &documents(), &[]);
    assert_eq!(attention.signal(), AttentionSignal::NeverOpened);
    assert!(!attention.is_partial());
    assert!(attention.documents_opened.is_empty());
    assert!(attention.first_seen.is_none());
}

/// An index view is an access: it is how "they looked but opened nothing" stays
/// distinguishable from "they never came".
#[test]
fn an_index_view_counts_as_an_access() {
    let events = vec![access("alice@example.com", None, now(), 1)];
    let attention = attention_for("alice@example.com", &documents(), &events);

    assert_eq!(attention.signal(), AttentionSignal::OpenedOnce);
    assert_eq!(attention.visits, 1);
    assert_eq!(attention.index_views, 1);
    assert!(attention.documents_opened.is_empty());
    assert!(
        !attention.is_partial(),
        "they opened no document, so this is not a partial read"
    );
}

/// "Unopened" means unopened out of what is *actually in the room*. A document
/// withdrawn last week is not something they failed to read.
#[test]
fn a_withdrawn_document_is_not_something_they_failed_to_read() {
    let events = vec![access(
        "alice@example.com",
        Some("artifact://deck@1"),
        now(),
        1,
    )];
    // The room now holds only the deck; financials were withdrawn.
    let current = vec!["artifact://deck@1".to_string()];

    let attention = attention_for("alice@example.com", &current, &events);
    assert!(attention.documents_unopened.is_empty());
    assert!(!attention.is_partial());
}

/// The record is possession, never identity. The type has no `identity` field at
/// all — a reader cannot casually write down a claim the system cannot support.
#[test]
fn the_record_names_the_token_not_the_person() {
    let event = access("alice@example.com", Some("artifact://deck@1"), now(), 1);
    let json = serde_json::to_string(&event).expect("serialise");

    assert!(json.contains("token_issued_to"));
    assert!(
        !json.contains("\"identity\""),
        "links get forwarded; asserting who opened it would make normal behaviour \
         look like an anomaly"
    );

    // And the room's notice says so, on the room itself rather than in a policy.
    assert!(ROOM_LOGGING_NOTICE.contains("forwarded"));
    assert!(ROOM_LOGGING_NOTICE.contains("records the link rather than the person"));
}

/// Dwell is best-effort, and `None` is the honest common case — no decision
/// should rest on a number the browser was never obliged to report honestly.
#[test]
fn dwell_is_optional_and_absent_by_default() {
    let event = access("alice@example.com", Some("artifact://deck@1"), now(), 1);
    assert!(event.dwell_ms.is_none());
    assert_eq!(event.user_agent_class, UserAgentClass::Unknown);
    assert_eq!(UserAgentClass::Desktop.as_str(), "desktop");
}

/// A single visit that views the index and then opens a document is ONE visit.
/// Counting events would report "returned to it" for somebody who came once and
/// clicked twice — the difference between interest and a single read.
#[test]
fn two_clicks_in_one_visit_are_not_a_return_visit() {
    let events = vec![
        access("alice@example.com", None, now(), 1),
        access(
            "alice@example.com",
            Some("artifact://deck@1"),
            now() + Duration::minutes(1),
            1,
        ),
    ];
    let attention = attention_for("alice@example.com", &documents(), &events);

    assert_eq!(attention.presentations, 2, "two events");
    assert_eq!(attention.visits, 1, "one visit");
    assert_eq!(
        attention.signal(),
        AttentionSignal::OpenedOnce,
        "they came once and clicked twice"
    );

    // A genuine return visit raises the sequence.
    let returned = vec![
        access("alice@example.com", None, now(), 1),
        access(
            "alice@example.com",
            Some("artifact://deck@1"),
            now() + Duration::minutes(1),
            1,
        ),
        access(
            "alice@example.com",
            Some("artifact://deck@1"),
            now() + Duration::days(3),
            2,
        ),
    ];
    assert_eq!(
        attention_for("alice@example.com", &documents(), &returned).signal(),
        AttentionSignal::OpenedRepeatedly
    );
}

/// One presentation is `OpenedOnce`, not `OpenedRepeatedly` — the boundary the
/// plan's two middle states turn on.
#[test]
fn one_presentation_is_not_a_return_visit() {
    let events = vec![access(
        "alice@example.com",
        Some("artifact://deck@1"),
        now(),
        1,
    )];
    let attention = attention_for("alice@example.com", &documents(), &events);
    assert_eq!(attention.signal(), AttentionSignal::OpenedOnce);
    assert_eq!(attention.first_seen, attention.last_seen);
    assert!(!attention.signal().is_delivery_question());
}

/// One surface reaches this module from outside, it is the reader side, and it
/// lives in another crate.
///
/// Pins the header the review found most misleading. It said *"there is still
/// no `share`, no link, no token and no reader-facing read path. Not 'not
/// wired' — **absent**"* while `magician-api` was serving exactly that path and
/// `magician-bin` was mounting its routes — so a reviewer was told the room
/// could disclose nothing at the one point where it can. The sibling crate is
/// why the scan has to be workspace-wide: a magician-only search returns the
/// empty answer the old header believed.
///
/// The other half is equally load-bearing: nothing OWNER-side calls any of it,
/// so a room only ever reaches a reader if something first opened it and
/// granted into it, and nothing does.
#[test]
fn the_known_owner_and_reader_surfaces_are_the_only_external_store_builders() {
    use magician::magician_v2::doc_wiring_scan::scan_workspace;

    const OWN_FILES: [&str; 10] = [
        "magician-learning/src/data_room/mod.rs",
        "magician-learning/src/data_room/store.rs",
        "magician-learning/src/data_room/types.rs",
        "magician-learning/src/data_room/tests.rs",
        "magician-learning/src/data_room/access_log.rs",
        "magician-learning/src/data_room/access_store.rs",
        "magician-learning/src/data_room/cycle.rs",
        "magician-learning/src/data_room/disclosure_bridge.rs",
        "magician-learning/src/data_room/follow_ups.rs",
        "magician-learning/src/data_room/sweep.rs",
    ];

    let data_rooms = scan_workspace("DataRoomStore::new", &OWN_FILES);
    assert!(data_rooms.files_searched > 100);
    assert_eq!(
        data_rooms.hits,
        [
            "magician-api/src/corrections_api.rs",
            "magician-api/src/data_room_api.rs",
            "magician-api/src/data_room_reader_api.rs",
            "magician-api/src/work_modules_api.rs",
            "magician-learning/src/outcome_learning/composition/mod.rs",
            "magician-learning/src/outcome_learning/composition/tests.rs",
            "magician-learning/src/retraction/tests.rs",
            "magician-media/src/obligation_sweeps/attention.rs",
            "magician-media/src/obligation_sweeps/mod.rs",
            "magician-media/src/obligation_sweeps/tests.rs",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
        "the set of data-room owner/reader constructors changed"
    );

    let access = scan_workspace("AccessStore::new", &OWN_FILES);
    assert!(access.files_searched > 100);
    assert_eq!(
        access.hits,
        [
            "magician-api/src/data_room_reader_api.rs",
            "magician-api/src/work_modules_api.rs",
            "magician-learning/src/outcome_learning/composition/mod.rs",
            "magician-learning/src/outcome_learning/composition/tests.rs",
            "magician-media/src/obligation_sweeps/attention.rs",
            "magician-media/src/obligation_sweeps/mod.rs",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
        "the set of data-room access owners changed"
    );

    // And that surface is mounted, so the path is live rather than compiled.
    let mounts = scan_workspace(
        "configure_data_room_reader_routes",
        &[
            "magician-api/src/data_room_reader_api.rs",
            "magician-learning/src/data_room/tests.rs",
        ],
    );
    assert_eq!(
        mounts.hits,
        vec!["magician-bin/src/main.rs".to_string()],
        "the reader routes are mounted somewhere else now; the header names `magician-bin`"
    );

    // Owner construction is deliberately present now for corrections,
    // obligations, retraction and outcome-learning. The exact lists above are
    // the reviewed boundary; this test no longer claims the room is inert.
}

/// §4: a room references an immutable revision, never "latest".
///
/// Refused at the write because there is nowhere later to catch it. A room
/// holding a moving pointer serves whoever opens it whatever the artifact says
/// today, and a reader comparing what they were shown against what was cleared
/// cannot tell they differ — stale is visible, swapped is not, which is why the
/// plan calls the second one worse.
#[test]
fn a_document_reference_must_name_a_revision() {
    let (_tmp, store, scope) = store();
    let assertions = register(&_tmp);
    let room_id = opened(&store, &scope, None);

    let refused = add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "artifact://deck",
        now(),
    )
    .expect_err("an unpinned reference must not enter a room");
    assert!(
        format!("{refused:#}").contains("must name a revision"),
        "the refusal has to say what to do about it: {refused:#}"
    );

    // READING is tolerant: the halves split on the LAST separator, because a
    // row already on file has to be made sense of however it was spelled.
    assert_eq!(
        super::types::split_document_ref("mailto:a@b.test@v9"),
        ("mailto:a@b.test", Some("v9"))
    );

    // WRITING is strict, and the asymmetry is the point. Splitting on the last
    // separator cannot tell an unpinned reference that merely CONTAINS one from
    // a pinned reference — `mailto:a@b.test` reads as `mailto:a` at revision
    // `b.test`, a revision matching nothing, so a retraction asking which rooms
    // carry a claim would silently fail to flag that room. A false negative on
    // a correction leaves a wrong figure in front of somebody.
    assert!(
        !super::types::names_a_revision("mailto:a@b.test@v9"),
        "more than one separator is a guess, and the write refuses to guess"
    );
    let refused = add(
        &store,
        &scope,
        &assertions,
        &room_id,
        "mailto:a@b.test@v9",
        now(),
    )
    .expect_err("an ambiguous reference must not enter a room");
    assert!(
        format!("{refused:#}").contains("separators"),
        "the refusal has to name the ambiguity: {refused:#}"
    );

    // A trailing separator names no revision, and a leading one names no
    // artifact. Neither is a pin, and reading either as one would let a
    // document in that pinned nothing.
    for bare in ["artifact://deck", "artifact://deck@", "@v1"] {
        assert!(
            !super::types::names_a_revision(bare),
            "`{bare}` names no revision"
        );
    }
    assert!(super::types::names_a_revision("artifact://deck@1"));

    let room = store.load(&scope, &room_id).expect("load").expect("room");
    assert!(room.documents.is_empty(), "nothing was added");
}
