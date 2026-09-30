//! §4's rule as behaviour: a retraction raises an event against every room
//! carrying the revision, and never swaps the file.

use chrono::TimeZone;

use crate::data_room::{DocumentVisibility, GrantDisclosure, OpenDataRoom};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{Audience, AudienceKind};
use magician::magician_v2::claim_manifest::ClaimBinding;
use magician::magician_v2::evidence::OutwardAssertionStore;

use super::*;

const CLAIM: &str = "claim-arr-2m";

struct Harness {
    _tmp: tempfile::TempDir,
    manifests: ClaimManifestStore,
    rooms: DataRoomStore,
    assertions: OutwardAssertionStore,
    manifest_scope: ClaimManifestScope,
    room_scope: DataRoomScope,
}

fn harness() -> Harness {
    let tmp = tempfile::tempdir().expect("temp dir");
    let layout = ArtifactV2Workspace::new(tmp.path());
    Harness {
        manifests: ClaimManifestStore::new(layout.clone()),
        rooms: DataRoomStore::new(layout.clone()),
        assertions: OutwardAssertionStore::new(layout),
        manifest_scope: ClaimManifestScope::new("anonymous", "default"),
        room_scope: DataRoomScope::new("anonymous", "default"),
        _tmp: tmp,
    }
}

fn at(day: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 1, 9, 0, 0).unwrap() + chrono::Duration::days(day)
}

fn audience_ref(id: &str) -> AudienceRef {
    AudienceRef::new(AudienceKind::Engagement, id)
}

/// Open a room for an audience and put one document in it.
fn room_with(h: &Harness, audience_id: &str, document_ref: &str, when: DateTime<Utc>) -> String {
    let audience = audience_ref(audience_id);
    let room = h
        .rooms
        .open(
            &h.room_scope,
            &OpenDataRoom {
                audience: audience.clone(),
                opened_by: "owner".to_string(),
                closes_at: None,
            },
            when,
        )
        .expect("room");
    h.rooms
        .add_document(
            &h.room_scope,
            &room.room_id,
            document_ref,
            DocumentVisibility::Everyone,
            "owner",
            &GrantDisclosure {
                assertions: &h.assertions,
                audience: &Audience::new(audience, vec!["reader@example.test".to_string()]),
                holders: &["reader@example.test".to_string()],
                disclosed_by: "owner",
            },
            when,
        )
        .expect("document");
    room.room_id
}

fn bind(h: &Harness, artifact: &str, revision: &str, claim: &str, when: DateTime<Utc>) {
    h.manifests
        .bind(
            &h.manifest_scope,
            artifact,
            revision,
            vec![ClaimBinding {
                claim_ref: claim.to_string(),
                evidence_refs: vec!["metric-1".to_string()],
            }],
            "owner",
            when,
        )
        .expect("bind");
}

fn sweep(h: &Harness, now: DateTime<Utc>) -> RetractionSweep {
    retraction_sweep(
        &h.manifests,
        &h.rooms,
        &h.manifest_scope,
        &h.room_scope,
        CLAIM,
        "correction-1",
        "owner",
        now,
    )
    .expect("sweep")
}

/// A room pinning the carrying revision is confirmed; one pinning a different
/// revision is genuinely unaffected and is not reported.
///
/// That second half is what §4's pinning BUYS: a corrected deck whose new
/// revision dropped the claim must not be flagged, or the list becomes noise.
#[test]
fn a_room_pinning_the_carrying_revision_is_confirmed_and_another_revision_is_not() {
    let h = harness();
    bind(&h, "artifact://deck", "r2", CLAIM, at(1));
    room_with(&h, "acme", "artifact://deck@r2", at(2));
    room_with(&h, "globex", "artifact://deck@r3", at(2));

    let result = sweep(&h, at(5));
    assert_eq!(result.rooms_seen, 2);
    assert_eq!(result.revisions_carrying, 1);
    assert_eq!(
        result.confirmed.len(),
        1,
        "only the pinned carrying revision"
    );
    assert_eq!(result.confirmed[0].audience, audience_ref("acme"));
    assert_eq!(result.confirmed[0].revision_ref.as_deref(), Some("r2"));
    assert_eq!(result.confirmed[0].exposure, Exposure::Current);
    assert!(
        result.unpinned.is_empty(),
        "both rooms named a revision, so neither is uncertain"
    );
    assert_eq!(
        result.proposed.len(),
        1,
        "one obligation per affected audience"
    );
    assert_eq!(result.proposed[0].direction, ObligationDirection::OwedByUs);
    assert_eq!(
        result.proposed[0].source_act_ref.as_deref(),
        Some("correction-1")
    );
}

/// An unpinned reference cannot enter a room at all any more.
///
/// §4's rule, enforced at the write since 2026-08-21. A room holding a moving
/// pointer serves whoever opens it whatever the artifact says today, and a
/// reader comparing what they were shown against what was cleared cannot tell
/// they differ — stale is visible, swapped is not.
#[test]
fn a_document_reference_naming_no_revision_is_refused_at_the_write() {
    let h = harness();
    let audience = audience_ref("acme");
    let room = h
        .rooms
        .open(
            &h.room_scope,
            &OpenDataRoom {
                audience: audience.clone(),
                opened_by: "owner".to_string(),
                closes_at: None,
            },
            at(1),
        )
        .expect("room");
    let refused = h.rooms.add_document(
        &h.room_scope,
        &room.room_id,
        "artifact://deck",
        DocumentVisibility::Everyone,
        "owner",
        &GrantDisclosure {
            assertions: &h.assertions,
            audience: &Audience::new(audience, vec!["reader@example.test".to_string()]),
            holders: &["reader@example.test".to_string()],
            disclosed_by: "owner",
        },
        at(1),
    );
    let error = refused.expect_err("an unpinned reference must not enter a room");
    assert!(
        format!("{error:#}").contains("must name a revision"),
        "the refusal has to say what to do about it: {error:#}"
    );
}

/// A reference the sweep can only guess at is NAMED, not skipped.
///
/// `add_document` refuses more than one separator, so this cannot be built
/// through the store — which is exactly why the rule is tested directly. The
/// data it protects against is a room written before that guard existed, the
/// same history the unpinned arm below exists for.
#[test]
fn a_reference_with_two_separators_is_a_guess_and_one_with_one_is_not() {
    assert!(
        super::reading_is_a_guess("mailto:a@b.test@v9"),
        "the last-separator reading would call the artifact `mailto:a@b.test`'s revision `v9`, \
         and a wrong split matches no carried artifact — so the room is silently skipped"
    );
    assert!(
        !super::reading_is_a_guess("artifact://deck@r2"),
        "exactly one separator is the form the write guard admits, and it reads exactly"
    );
    assert!(
        !super::reading_is_a_guess("artifact://deck"),
        "no separator is unpinned, not ambiguous — reported apart, with its own certainty"
    );
}

/// The uncertain arm still says what it is, for the history that predates the
/// guard.
///
/// Rows written before the write refused unpinned references still hold them,
/// so the arm is reachable by data even though no new write can create it —
/// which is why it is exercised here directly rather than through the store.
/// A history that predates a rule does not retroactively obey it, and a sweep
/// that assumed otherwise would answer confidently about rooms it cannot read.
#[test]
fn an_unpinned_row_from_before_the_guard_is_reported_apart_and_says_why() {
    let sweep = RetractionSweep {
        unpinned: vec![AffectedRoom {
            room_id: "room-1".to_string(),
            audience: audience_ref("acme"),
            document_ref: "artifact://deck".to_string(),
            artifact_ref: "artifact://deck".to_string(),
            revision_ref: None,
            certainty: Certainty::Unpinned,
            exposure: Exposure::Current,
        }],
        rooms_seen: 1,
        revisions_carrying: 1,
        ..RetractionSweep::default()
    };
    assert_eq!(sweep.affected(), 1);
    assert!(sweep.confirmed.is_empty());

    let proposed = super::proposals(&sweep, CLAIM, "correction-1", "owner", at(5));
    assert_eq!(proposed.len(), 1);
    assert!(
        proposed[0].what.contains("name no revision at all"),
        "the uncertainty has to reach the person acting on it: {}",
        proposed[0].what
    );
    assert!(
        !proposed[0].what.contains("0 document"),
        "a count of zero reads as nothing to do, for the case that most needs looking at: {}",
        proposed[0].what
    );
}

/// A withdrawn document still counts, and the proposal says why acting now
/// changes nothing.
///
/// §4: *"revocation blocks future access and cannot recall a prior download.
/// Nothing here should be described as if it could."* A sweep that filtered
/// withdrawn documents out would answer "nobody is holding it" about people
/// who are.
#[test]
fn a_withdrawn_document_is_past_exposure_and_still_owes_a_conversation() {
    let h = harness();
    bind(&h, "artifact://deck", "r2", CLAIM, at(1));
    let room_id = room_with(&h, "acme", "artifact://deck@r2", at(2));
    h.rooms
        .withdraw_document(&h.room_scope, &room_id, "artifact://deck@r2", at(3))
        .expect("withdraw");

    let result = sweep(&h, at(5));
    assert_eq!(
        result.confirmed.len(),
        1,
        "a withdrawn document is not a document nobody read"
    );
    assert_eq!(result.confirmed[0].exposure, Exposure::Past);
    assert!(
        result.proposed[0].what.contains("not recalled"),
        "an owner reading `revoke it` for a closed door would think the problem was solved: {}",
        result.proposed[0].what
    );
}

/// Two rooms shared with one counterparty produce ONE conversation.
#[test]
fn one_obligation_per_audience_not_per_room() {
    let h = harness();
    bind(&h, "artifact://deck", "r2", CLAIM, at(1));
    bind(&h, "artifact://memo", "v1", CLAIM, at(1));
    // Both documents land in the SAME room, because a room is derived per
    // audience — which is exactly why grouping by audience is the right unit.
    let audience = audience_ref("acme");
    let room = h
        .rooms
        .open(
            &h.room_scope,
            &OpenDataRoom {
                audience: audience.clone(),
                opened_by: "owner".to_string(),
                closes_at: None,
            },
            at(2),
        )
        .expect("room");
    for document in ["artifact://deck@r2", "artifact://memo@v1"] {
        h.rooms
            .add_document(
                &h.room_scope,
                &room.room_id,
                document,
                DocumentVisibility::Everyone,
                "owner",
                &GrantDisclosure {
                    assertions: &h.assertions,
                    audience: &Audience::new(
                        audience.clone(),
                        vec!["reader@example.test".to_string()],
                    ),
                    holders: &["reader@example.test".to_string()],
                    disclosed_by: "owner",
                },
                at(2),
            )
            .expect("document");
    }

    let result = sweep(&h, at(5));
    assert_eq!(result.confirmed.len(), 2, "both documents carry it");
    assert_eq!(
        result.proposed.len(),
        1,
        "one counterparty, one conversation"
    );
    assert!(
        result.proposed[0].what.contains("2 document(s)"),
        "{}",
        result.proposed[0].what
    );
}

/// A claim nobody bound reaches nothing, and says how much it looked at.
///
/// `rooms_seen: 0` and `rooms_seen: 300` are different facts behind the same
/// "nothing affected".
#[test]
fn a_claim_no_revision_carries_reaches_nothing_and_reports_the_denominator() {
    let h = harness();
    room_with(&h, "acme", "artifact://deck@r2", at(2));

    let result = sweep(&h, at(5));
    assert_eq!(result.revisions_carrying, 0);
    assert_eq!(result.affected(), 0);
    assert!(result.proposed.is_empty());
    assert_eq!(
        result.rooms_seen, 1,
        "it looked at one room and found nothing"
    );
}

/// A retraction that names no correction is refused.
///
/// The obligation points at the correction; one pointing nowhere cannot be
/// acted on.
#[test]
fn a_retraction_must_name_the_correction() {
    let h = harness();
    let refused = retraction_sweep(
        &h.manifests,
        &h.rooms,
        &h.manifest_scope,
        &h.room_scope,
        CLAIM,
        "   ",
        "owner",
        at(5),
    );
    assert!(refused.is_err());
}

/// The pin splits on the LAST separator, so an artifact reference containing
/// one keeps its own.
#[test]
fn the_pin_is_the_trailing_segment_and_a_bare_separator_pins_nothing() {
    use crate::data_room::split_document_ref;

    assert_eq!(
        split_document_ref("artifact://deck@r2"),
        ("artifact://deck", Some("r2"))
    );
    assert_eq!(
        split_document_ref("mailto:a@b.test@v9"),
        ("mailto:a@b.test", Some("v9")),
        "the LAST separator is the pin"
    );
    assert_eq!(
        split_document_ref("artifact://deck"),
        ("artifact://deck", None)
    );
    assert_eq!(
        split_document_ref("artifact://deck@"),
        ("artifact://deck@", None)
    );
    assert_eq!(split_document_ref("@r2"), ("@r2", None));
}
