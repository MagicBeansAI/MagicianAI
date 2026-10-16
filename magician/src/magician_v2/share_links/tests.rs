//! The share-link contract, as behaviour: *"one identity, one link, expiring,
//! revocable, bound to the engagement"* — the binding widened to any audience,
//! the resource to any `resource_ref`.

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::{Audience, AudienceRef};

use super::{
    IssueShareLink, Presentation, PresentationRefused, ShareLinkScope, ShareLinkState,
    ShareLinkStore,
};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn expiry() -> DateTime<Utc> {
    now() + Duration::days(7)
}

fn fixture() -> (tempfile::TempDir, ShareLinkStore, ShareLinkScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = ShareLinkStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, ShareLinkScope::new("anonymous", "default"))
}

fn buyers_ref() -> AudienceRef {
    AudienceRef::engagement("eng-1")
}

fn buyers() -> Audience {
    Audience::new(buyers_ref(), vec!["ana".to_string(), "bo".to_string()])
}

fn grant(issued_to: &str, secret: &str) -> IssueShareLink {
    IssueShareLink {
        resource_ref: "room-1".to_string(),
        audience: buyers_ref(),
        issued_to: issued_to.to_string(),
        secret: secret.to_string(),
        expires_at: expiry(),
    }
}

fn admitted(issued_to: &str, sequence: u32) -> Presentation {
    Presentation {
        issued_to: issued_to.to_string(),
        sequence,
    }
}

/// "Expiring" is not optional: a possession-based grant that never lapses is
/// the blanket-yes failure. The type already refuses `None` by having no
/// `Option`; this pins the runtime half — an expiry at or before issuance is
/// a grant born dead, and issuing it would hide a caller bug.
#[test]
fn issuance_refuses_an_expiry_that_is_not_ahead_of_now() {
    let (_tmp, store, scope) = fixture();

    let mut born_dead = grant("ana", "secret-a");
    born_dead.expires_at = now();
    assert!(
        store.issue(&scope, &born_dead, now()).is_err(),
        "expiry is inclusive: expiring at issuance is expired at issuance"
    );

    born_dead.expires_at = now() - Duration::hours(1);
    assert!(store.issue(&scope, &born_dead, now()).is_err());

    assert!(
        store
            .for_resource(&scope, "room-1")
            .expect("read")
            .is_empty(),
        "a refused issuance leaves no record"
    );
}

/// Fail closed on absent material: a grant that names no resource, no
/// relationship, no identity or no secret grants nothing.
#[test]
fn issuance_refuses_blank_material() {
    let (_tmp, store, scope) = fixture();

    let mut unnamed_resource = grant("ana", "secret-a");
    unnamed_resource.resource_ref = "  ".to_string();
    assert!(store.issue(&scope, &unnamed_resource, now()).is_err());

    let mut unnamed_audience = grant("ana", "secret-a");
    unnamed_audience.audience = AudienceRef::engagement("  ");
    assert!(store.issue(&scope, &unnamed_audience, now()).is_err());

    let unnamed_identity = grant("  ", "secret-a");
    assert!(store.issue(&scope, &unnamed_identity, now()).is_err());

    let blank_secret = grant("ana", "   ");
    assert!(store.issue(&scope, &blank_secret, now()).is_err());

    assert!(store
        .for_resource(&scope, "room-1")
        .expect("read")
        .is_empty());
}

/// "One identity, one link": a second live grant for the same identity is
/// refused so revocation always has exactly one target. The constraint is per
/// identity per resource — another identity on the same resource, and the
/// same identity on another resource, are separate grants.
#[test]
fn one_identity_holds_one_link() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("first");

    assert!(
        store
            .issue(&scope, &grant("ana", "secret-b"), now())
            .is_err(),
        "a second live link would give revocation two targets"
    );

    store
        .issue(&scope, &grant("bo", "secret-c"), now())
        .expect("another identity on the same resource");
    let mut other_resource = grant("ana", "secret-d");
    other_resource.resource_ref = "room-2".to_string();
    store
        .issue(&scope, &other_resource, now())
        .expect("the same identity on another resource");

    assert_eq!(store.for_resource(&scope, "room-1").expect("read").len(), 2);
    assert_eq!(store.for_resource(&scope, "room-2").expect("read").len(), 1);
}

/// Derived ids make retries resume instead of duplicate: the identical
/// issuance replayed — a retried call whose response was lost — is one grant,
/// and the replay must not move when it was issued or mint a new credential.
#[test]
fn replaying_the_same_issuance_resumes_the_same_link() {
    let (_tmp, store, scope) = fixture();
    let first = store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("first");
    let again = store
        .issue(
            &scope,
            &grant("ana", "secret-a"),
            now() + Duration::hours(2),
        )
        .expect("replay");

    assert_eq!(again, first);
    assert_eq!(store.for_resource(&scope, "room-1").expect("read").len(), 1);
}

/// A secret must name exactly one credential on a resource, or a presentation
/// could not be attributed honestly — the access log would not know whose
/// visit it saw.
#[test]
fn a_secret_names_exactly_one_credential() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "shared-secret"), now())
        .expect("ana");
    assert!(store
        .issue(&scope, &grant("bo", "shared-secret"), now())
        .is_err());
}

/// "Revocable" plus "one link": rotation revokes the old credential and
/// issues the new one in a single call — the old secret stops presenting, the
/// new one presents.
#[test]
fn rotation_kills_the_old_secret_and_arms_the_new_one() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "old-secret"), now())
        .expect("issue");
    assert_eq!(
        store
            .present(&scope, "room-1", "old-secret", &buyers(), now())
            .expect("io"),
        Ok(admitted("ana", 1))
    );

    let rotated = store
        .rotate(
            &scope,
            &grant("ana", "new-secret"),
            now() + Duration::hours(1),
        )
        .expect("rotate");
    assert_eq!(rotated.revoked_at, None);
    assert_eq!(rotated.issued_at, now() + Duration::hours(1));
    assert_eq!(
        rotated.presentations, 1,
        "the slot's presentation count carries over"
    );
    assert_eq!(
        rotated.rotation_of,
        Some(super::hash_secret("old-secret")),
        "the successor carries explicit rotation evidence naming its predecessor"
    );

    // The rotated-away secret no longer names any current credential: the
    // slot was superseded, so the honest answer is that the secret is unknown.
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "old-secret",
                &buyers(),
                now() + Duration::hours(2)
            )
            .expect("io"),
        Err(PresentationRefused::UnknownSecret)
    );
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "new-secret",
                &buyers(),
                now() + Duration::hours(2)
            )
            .expect("io"),
        Ok(admitted("ana", 2))
    );
}

/// Rotation is a replacement, not an upsert: with nothing live it refuses — a
/// caller who believes an old credential was just invalidated must find out
/// none existed — and rotating to the very secret being rotated away would
/// leave the credential the call claims to invalidate still presentable.
#[test]
fn rotation_requires_a_live_link_and_a_genuinely_new_secret() {
    let (_tmp, store, scope) = fixture();
    assert!(
        store
            .rotate(&scope, &grant("ana", "secret-a"), now())
            .is_err(),
        "nothing to rotate"
    );

    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");
    assert!(
        store
            .rotate(
                &scope,
                &grant("ana", "secret-a"),
                now() + Duration::hours(1)
            )
            .is_err(),
        "the replacement must actually replace"
    );

    // The refused rotation changed nothing: the standing credential still
    // presents, as its first presentation.
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "secret-a",
                &buyers(),
                now() + Duration::hours(2)
            )
            .expect("io"),
        Ok(admitted("ana", 1))
    );
}

/// "Revocable", forward-only: revocation prevents future presentation, does
/// not recall what was already fetched, and the first revocation is the
/// revocation — a later call never moves the recorded time of the kill.
#[test]
fn revocation_is_forward_only_and_idempotent() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");
    store
        .present(&scope, "room-1", "secret-a", &buyers(), now())
        .expect("io")
        .expect("admitted");

    let killed_at = now() + Duration::hours(1);
    let revoked = store
        .revoke(&scope, "room-1", "ana", killed_at)
        .expect("revoke");
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].revoked_at, Some(killed_at));
    assert_eq!(
        revoked[0].presentations, 1,
        "the presentation already made stays on the record"
    );

    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "secret-a",
                &buyers(),
                killed_at + Duration::minutes(1)
            )
            .expect("io"),
        Err(PresentationRefused::Revoked)
    );

    let again = store
        .revoke(&scope, "room-1", "ana", now() + Duration::days(3))
        .expect("again");
    assert_eq!(
        again[0].revoked_at,
        Some(killed_at),
        "the first revocation stands"
    );

    // Revoking an identity that holds nothing is the asked-for end-state
    // already holding; a removal is where fail-closed cuts the other way.
    assert!(store
        .revoke(&scope, "room-1", "stranger", now())
        .expect("no-op")
        .is_empty());
}

/// The refusal reason is honest for the audit log: a credential the owner
/// killed refuses as revoked, and a string that never was a credential
/// refuses as unknown. What to reveal outward is the transport layer's call.
#[test]
fn a_revoked_secret_refuses_as_revoked_not_unknown() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");
    store
        .revoke(&scope, "room-1", "ana", now() + Duration::hours(1))
        .expect("revoke");

    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "secret-a",
                &buyers(),
                now() + Duration::hours(2)
            )
            .expect("io"),
        Err(PresentationRefused::Revoked)
    );
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "never-issued",
                &buyers(),
                now() + Duration::hours(2)
            )
            .expect("io"),
        Err(PresentationRefused::UnknownSecret)
    );
}

/// "Expiring", inclusively, like every expiry in this codebase: expiring at
/// noon means expired at noon.
#[test]
fn expiry_is_inclusive() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "secret-a",
                &buyers(),
                expiry() - Duration::seconds(1)
            )
            .expect("io"),
        Ok(admitted("ana", 1))
    );
    assert_eq!(
        store
            .present(&scope, "room-1", "secret-a", &buyers(), expiry())
            .expect("io"),
        Err(PresentationRefused::Expired)
    );
}

/// "Bound to the engagement", generalised: access derives from the
/// relationship and ends with it. An ended audience refuses even an identity
/// it still lists — "was listed" must never become "may see".
#[test]
fn an_ended_audience_refuses_even_a_listed_identity() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    let ends = now() + Duration::days(1);
    let ending = buyers().expiring_at(ends);
    assert_eq!(
        store
            .present(&scope, "room-1", "secret-a", &ending, now())
            .expect("io"),
        Ok(admitted("ana", 1))
    );
    // Audience currency is inclusive too: ending at the instant is ended at
    // the instant.
    assert_eq!(
        store
            .present(&scope, "room-1", "secret-a", &ending, ends)
            .expect("io"),
        Err(PresentationRefused::AudienceEnded)
    );
}

/// The grant binds to one relationship, not to whoever happens to be in the
/// roster: the same people under a different audience — or the same audience
/// id under a different kind — are a different relationship.
#[test]
fn the_grant_binds_to_one_relationship_not_just_its_members() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    let other_deal = Audience::new(
        AudienceRef::engagement("eng-2"),
        vec!["ana".to_string(), "bo".to_string()],
    );
    assert_eq!(
        store
            .present(&scope, "room-1", "secret-a", &other_deal, now())
            .expect("io"),
        Err(PresentationRefused::AudienceMismatch)
    );

    // Same id, different kind: `as_key` keeps these apart, and so must the gate.
    let same_id_other_kind = Audience::new(
        AudienceRef::account("eng-1"),
        vec!["ana".to_string(), "bo".to_string()],
    );
    assert_eq!(
        store
            .present(&scope, "room-1", "secret-a", &same_id_other_kind, now())
            .expect("io"),
        Err(PresentationRefused::AudienceMismatch)
    );
}

/// Membership is checked live at presentation, against the supplied roster:
/// dropping someone from the audience ends their access without touching the
/// link. And an audience naming nobody admits nobody — the empty roster must
/// fail closed, never vacuously pass a member predicate.
#[test]
fn an_identity_the_audience_no_longer_names_is_refused() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    let without_ana = Audience::new(buyers_ref(), vec!["bo".to_string()]);
    assert_eq!(
        store
            .present(&scope, "room-1", "secret-a", &without_ana, now())
            .expect("io"),
        Err(PresentationRefused::IdentityNotAdmitted)
    );

    let nobody = Audience::new(buyers_ref(), Vec::new());
    assert_eq!(
        store
            .present(&scope, "room-1", "secret-a", &nobody, now())
            .expect("io"),
        Err(PresentationRefused::IdentityNotAdmitted)
    );
}

/// `sequence` numbers presentations — first, second, nth — per grant slot.
/// It is **not** the access log's visit number: the reader surface presents the
/// credential on every request and numbers the visit from the access lane. What
/// this module itself promises, pinned here, is the presentation count.
#[test]
fn sequence_numbers_presentations_one_two_three() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    for presentation in 1..=3u32 {
        assert_eq!(
            store
                .present(
                    &scope,
                    "room-1",
                    "secret-a",
                    &buyers(),
                    now() + Duration::hours(i64::from(presentation)),
                )
                .expect("io"),
            Ok(admitted("ana", presentation))
        );
    }

    let held = store.for_resource(&scope, "room-1").expect("read");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].presentations, 3);
}

/// The presentation count belongs to the identity's grant slot, not to the
/// secret in their hands: rotating the credential must not make a third
/// presentation read as a first, or the record of how often this identity's
/// link has been used restarts every time the owner replaces the URL.
#[test]
fn the_presentation_count_survives_rotation() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "old-secret"), now())
        .expect("issue");
    store
        .present(&scope, "room-1", "old-secret", &buyers(), now())
        .expect("io")
        .expect("first presentation");
    store
        .present(
            &scope,
            "room-1",
            "old-secret",
            &buyers(),
            now() + Duration::hours(1),
        )
        .expect("io")
        .expect("second presentation");

    store
        .rotate(
            &scope,
            &grant("ana", "new-secret"),
            now() + Duration::hours(2),
        )
        .expect("rotate");

    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "new-secret",
                &buyers(),
                now() + Duration::hours(3)
            )
            .expect("io"),
        Ok(admitted("ana", 3))
    );
}

/// Grants are per scope and per resource: the same secret opens nothing in
/// another workspace and nothing on another resource — an unknown secret is
/// all either ever sees.
#[test]
fn grants_are_per_scope_and_per_resource() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    let elsewhere = ShareLinkScope::new("someone-else", "default");
    assert_eq!(
        store
            .present(&elsewhere, "room-1", "secret-a", &buyers(), now())
            .expect("io"),
        Err(PresentationRefused::UnknownSecret)
    );
    assert!(store
        .for_resource(&elsewhere, "room-1")
        .expect("read")
        .is_empty());

    assert_eq!(
        store
            .present(&scope, "room-2", "secret-a", &buyers(), now())
            .expect("io"),
        Err(PresentationRefused::UnknownSecret)
    );
}

/// Where a link stands is derived from the clock at read time, never stored:
/// an expired link must read as expired even if nothing ran. The owner's
/// explicit kill outranks the clock's verdict.
#[test]
fn link_state_is_derived_from_the_clock() {
    let (_tmp, store, scope) = fixture();
    let link = store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    assert_eq!(link.state(now()), ShareLinkState::Live);
    assert!(link.is_live(now()));
    assert_eq!(
        link.state(expiry()),
        ShareLinkState::Expired,
        "inclusive: expiring at the instant is expired at the instant"
    );

    let revoked = store
        .revoke(&scope, "room-1", "ana", now() + Duration::hours(1))
        .expect("revoke");
    assert_eq!(
        revoked[0].state(expiry() + Duration::days(1)),
        ShareLinkState::Revoked,
        "the owner's act outranks the clock"
    );
    assert_eq!(
        store
            .live_link(&scope, "room-1", "ana", now() + Duration::hours(2))
            .expect("read"),
        None
    );
}

/// Pins the fail-open read the adversarial review found: an I/O fault on an
/// EXISTING log must propagate, never fold to an empty store. Before the fix
/// (`Err(_) => Ok(None)`) an unreadable log made `revoke` report its
/// documented no-op success while revoking nothing, let a second issue pass
/// the one-identity-one-link guard vacuously, and made `present` record a
/// disk fault as `UnknownSecret` — the exact lie its audit contract forbids.
#[test]
fn an_unreadable_log_is_an_error_not_an_empty_store() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    // Fault the log without erasing it: bytes no UTF-8 read accepts, so the
    // file plainly exists and plainly cannot be read.
    let path = store.log_path(&scope, "room-1");
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open log");
    std::io::Write::write_all(&mut log, &[0xff, 0xfe, 0xff]).expect("fault the log");
    drop(log);

    let refused = store.revoke(&scope, "room-1", "ana", now() + Duration::hours(1));
    let message = format!("{:#}", refused.expect_err("revoke must surface the fault"));
    assert!(
        message.contains("an unreadable log must never be treated as an empty one"),
        "the error names the failure mode, got: {message}"
    );
    assert!(
        store
            .present(&scope, "room-1", "secret-a", &buyers(), now())
            .is_err(),
        "a disk fault is a storage error, never an inner refusal"
    );
    assert!(
        store
            .issue(&scope, &grant("ana", "secret-b"), now())
            .is_err(),
        "the one-identity guard must not pass vacuously over a log it could not read"
    );
}

/// Pins the torn-tail rule the honest-read fix brought in via
/// `jsonl::parse_log_lines`: a crash mid-append leaves exactly one torn FINAL
/// line, and that append is an operation that never happened — the fold reads
/// everything before it instead of refusing the whole log.
#[test]
fn a_torn_final_line_reads_as_an_append_that_never_happened() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue");

    // A revocation whose append tore mid-line: the kill never completed.
    let path = store.log_path(&scope, "room-1");
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open log");
    std::io::Write::write_all(&mut log, b"{\"record\":\"revoked\",\"link_id\":\"shl-")
        .expect("tear the tail");
    drop(log);

    let held = store
        .for_resource(&scope, "room-1")
        .expect("a torn tail is not corruption");
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0].revoked_at, None,
        "the torn revocation never happened"
    );
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "secret-a",
                &buyers(),
                now() + Duration::hours(1)
            )
            .expect("io"),
        Ok(admitted("ana", 1)),
        "the credential the torn kill never reached still presents"
    );
}

/// Pins the dead-slot resurrection the review found: issuing a secret whose
/// credential in the same slot is dead must refuse, not append a fresh live
/// record. Before the fix the live-link guard could not see a revoked or
/// expired credential and the cross-slot secret guard excluded the slot's own
/// id, so a replayed issue after the owner's kill re-armed the leaked URL,
/// and a re-issue after expiry silently extended it — the exact extend
/// rotation refuses by design.
#[test]
fn a_dead_slot_never_rearms_its_own_secret() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "leaked"), now())
        .expect("issue");
    store
        .revoke(&scope, "room-1", "ana", now() + Duration::hours(1))
        .expect("the owner kills the leaked URL");

    // The client retries the identical issue after the kill: refused, and the
    // killed credential still refuses as revoked — not resurrected.
    let retried = store.issue(&scope, &grant("ana", "leaked"), now() + Duration::hours(2));
    let message = format!(
        "{:#}",
        retried.expect_err("a killed credential is never re-armed")
    );
    assert!(
        message.contains("revoked"),
        "the refusal names the slot's state, got: {message}"
    );
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "leaked",
                &buyers(),
                now() + Duration::hours(3)
            )
            .expect("io"),
        Err(PresentationRefused::Revoked)
    );

    // A fresh secret re-opens the slot: it is the same-secret re-arm that is
    // refused, never the slot itself.
    let fresh = store
        .issue(&scope, &grant("ana", "fresh"), now() + Duration::hours(4))
        .expect("a fresh secret re-opens the slot");
    assert_eq!(fresh.revoked_at, None);

    // The expiry half: the same secret cannot re-issue past its own lapse —
    // that would be the silent extend the no-extend rule exists to prevent.
    store
        .issue(&scope, &grant("bo", "lapsing"), now())
        .expect("bo");
    let after_lapse = expiry() + Duration::hours(1);
    let mut extend = grant("bo", "lapsing");
    extend.expires_at = after_lapse + Duration::days(7);
    let extended = store.issue(&scope, &extend, after_lapse);
    let message = format!(
        "{:#}",
        extended.expect_err("a lapsed credential is never extended")
    );
    assert!(
        message.contains("expired"),
        "the refusal names the slot's state, got: {message}"
    );
}

/// Pins the rotate rebind the review found: a rotate request bound to a
/// different audience than the live link's must refuse before anything is
/// appended. Before the fix the live link was found by identity alone, so a
/// rebind revoked the old slot and issued into a NEW slot whose carried
/// presentation count was zero — the record of how often that identity's link
/// had been used would have restarted at nothing.
#[test]
fn rotation_refuses_a_rebind_to_a_different_audience() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "old-secret"), now())
        .expect("issue");
    assert_eq!(
        store
            .present(&scope, "room-1", "old-secret", &buyers(), now())
            .expect("io"),
        Ok(admitted("ana", 1))
    );

    let mut rebind = grant("ana", "new-secret");
    rebind.audience = AudienceRef::account("acme");
    let refused = store.rotate(&scope, &rebind, now() + Duration::hours(1));
    let message = format!(
        "{:#}",
        refused.expect_err("a rebind is a different operation")
    );
    assert!(
        message.contains("rebind"),
        "the refusal says what to do instead, got: {message}"
    );

    // Refused before any append: the live credential is intact, no new slot
    // appeared, and the identity's presentation numbering continues where it was.
    assert_eq!(store.for_resource(&scope, "room-1").expect("read").len(), 1);
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "old-secret",
                &buyers(),
                now() + Duration::hours(2)
            )
            .expect("io"),
        Ok(admitted("ana", 2))
    );
}

/// Pins the completed-rotation replay the review found: a retried rotate
/// whose first attempt landed both records must resume — return the live
/// link — not fail the same-secret refusal, which in this interleaving
/// asserts a falsehood (the rotated-away credential is already dead). The
/// live credential's explicit `rotation_of` mark — written only by rotate —
/// separates the replay from a first-time rotate to the standing secret,
/// which still refuses.
#[test]
fn replaying_a_completed_rotation_resumes_instead_of_erroring() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "old-secret"), now())
        .expect("issue");
    store
        .present(&scope, "room-1", "old-secret", &buyers(), now())
        .expect("io")
        .expect("first presentation");

    let rotated = store
        .rotate(
            &scope,
            &grant("ana", "new-secret"),
            now() + Duration::hours(1),
        )
        .expect("rotate");

    // The response was lost; the client replays the identical rotation.
    let resumed = store
        .rotate(
            &scope,
            &grant("ana", "new-secret"),
            now() + Duration::hours(2),
        )
        .expect("the completed rotation resumes");
    assert_eq!(
        resumed, rotated,
        "the replay returns the live link, minting nothing"
    );

    // The replay appended nothing: the log holds the issue, its one
    // presentation and the one rotation's two records — and only those.
    let raw = std::fs::read_to_string(store.log_path(&scope, "room-1")).expect("raw log");
    assert_eq!(raw.lines().count(), 4);

    // The old secret stays dead and the identity's presentation numbering
    // continues.
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "old-secret",
                &buyers(),
                now() + Duration::hours(3)
            )
            .expect("io"),
        Err(PresentationRefused::UnknownSecret)
    );
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "new-secret",
                &buyers(),
                now() + Duration::hours(4)
            )
            .expect("io"),
        Ok(admitted("ana", 2))
    );
}

/// Pins the dead-hash amnesia the review found: the dead-slot guard compared
/// only the slot's CURRENT credential hash, so an OLDER revoked secret — here
/// `gen-a`, rotated away before its successor was killed — could be re-armed
/// by a replayed issue, resurrecting a URL the owner had already retired.
#[test]
fn a_rotated_away_secret_never_rearms_after_its_successor_dies() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "gen-a"), now())
        .expect("issue");
    store
        .rotate(&scope, &grant("ana", "gen-b"), now() + Duration::hours(1))
        .expect("rotate: gen-a dies");
    store
        .revoke(&scope, "room-1", "ana", now() + Duration::hours(2))
        .expect("the owner kills gen-b");

    let replayed = store.issue(&scope, &grant("ana", "gen-a"), now() + Duration::hours(3));
    let message = format!(
        "{:#}",
        replayed.expect_err("a killed credential may never live again")
    );
    assert!(message.contains("never live again"), "got: {message}");

    // The refusal appended nothing: the slot still holds the revoked gen-b
    // credential, gen-a still names no current credential, and gen-b still
    // refuses as the owner's kill.
    let held = store.for_resource(&scope, "room-1").expect("read");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].revoked_at, Some(now() + Duration::hours(2)));
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "gen-a",
                &buyers(),
                now() + Duration::hours(4)
            )
            .expect("io"),
        Err(PresentationRefused::UnknownSecret)
    );
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "gen-b",
                &buyers(),
                now() + Duration::hours(4)
            )
            .expect("io"),
        Err(PresentationRefused::Revoked)
    );
}

/// Pins the multi-generation half of the dead-hash amnesia: across the chain
/// A -> B -> C every superseded secret stays dead. A replayed issue of A or B
/// refuses, a rotate TO a dead generation refuses before anything is
/// appended, and the live credential survives every refused call untouched.
#[test]
fn every_generation_of_a_rotation_chain_stays_dead() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "gen-a"), now())
        .expect("issue");
    store
        .rotate(&scope, &grant("ana", "gen-b"), now() + Duration::hours(1))
        .expect("a -> b");
    store
        .rotate(&scope, &grant("ana", "gen-c"), now() + Duration::hours(2))
        .expect("b -> c");

    for dead in ["gen-a", "gen-b"] {
        let replayed = store.issue(&scope, &grant("ana", dead), now() + Duration::hours(3));
        let message = format!(
            "{:#}",
            replayed.expect_err("no dead generation ever re-arms")
        );
        assert!(message.contains("never live again"), "got: {message}");
    }

    let rotated = store.rotate(&scope, &grant("ana", "gen-b"), now() + Duration::hours(3));
    let message = format!(
        "{:#}",
        rotated.expect_err("rotating to a dead generation refuses")
    );
    assert!(message.contains("never live again"), "got: {message}");

    // Issue + two rotations wrote five records; every refusal appended none,
    // so the failed rotate never revoked the live credential on its way out.
    let raw = std::fs::read_to_string(store.log_path(&scope, "room-1")).expect("raw log");
    assert_eq!(raw.lines().count(), 5);
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "gen-c",
                &buyers(),
                now() + Duration::hours(4)
            )
            .expect("io"),
        Ok(admitted("ana", 1))
    );
}

/// Pins the forged rotation evidence the review found: rotation used to be
/// inferred from a timestamp coincidence — predecessor `revoked_at` equal to
/// successor `issued_at` — which a legitimate same-clock revoke-plus-issue
/// forges, making a FIRST-TIME rotate to the standing secret silently resume.
/// Rotation is explicit now (`rotation_of`), so the forgery hits the
/// same-secret refusal instead.
#[test]
fn a_same_clock_revoke_and_issue_is_not_rotation_evidence() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "first"), now())
        .expect("issue");
    let same_instant = now() + Duration::hours(1);
    store
        .revoke(&scope, "room-1", "ana", same_instant)
        .expect("revoke");
    let standing = store
        .issue(&scope, &grant("ana", "second"), same_instant)
        .expect("a fresh secret at the same clock instant is a plain issue");
    assert_eq!(
        standing.rotation_of, None,
        "a plain issue carries no rotation evidence"
    );

    // The forged coincidence is in place; a first-time rotate carrying the
    // standing secret must refuse — resuming would claim `second` was just
    // invalidated when it is still the live credential.
    let refused = store.rotate(&scope, &grant("ana", "second"), now() + Duration::hours(2));
    let message = format!("{:#}", refused.expect_err("a coincidence is not evidence"));
    assert!(message.contains("being rotated away"), "got: {message}");

    // The refusal appended nothing and the standing credential still
    // presents, as its first presentation.
    let raw = std::fs::read_to_string(store.log_path(&scope, "room-1")).expect("raw log");
    assert_eq!(raw.lines().count(), 3);
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "second",
                &buyers(),
                now() + Duration::hours(3)
            )
            .expect("io"),
        Ok(admitted("ana", 1))
    );
}

/// Pins the compat half of explicit rotation: an `Issued` record written
/// before `rotation_of` existed carries no such field, and the fold must
/// parse it as a non-rotation (`None`) instead of refusing the whole log as
/// corrupt.
#[test]
fn an_issued_record_without_rotation_of_still_parses() {
    let (_tmp, store, scope) = fixture();
    store
        .issue(&scope, &grant("ana", "secret-a"), now())
        .expect("issue creates the log");

    // Hand-write a pre-`rotation_of` Issued record for a second identity —
    // field for field what the previous code serialized.
    let request = grant("bo", "secret-b");
    let legacy = serde_json::json!({
        "record": "issued",
        "link_id": super::derive_link_id(&scope, &request),
        "resource_ref": "room-1",
        "audience": serde_json::to_value(buyers_ref()).expect("audience"),
        "issued_to": "bo",
        "secret_hash": super::hash_secret("secret-b"),
        "issued_at": serde_json::to_value(now()).expect("issued_at"),
        "expires_at": serde_json::to_value(expiry()).expect("expires_at"),
        "revoked_at": serde_json::Value::Null,
        "presentations": 0
    });
    let path = store.log_path(&scope, "room-1");
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open log");
    std::io::Write::write_all(&mut log, format!("{legacy}\n").as_bytes()).expect("legacy line");
    drop(log);

    let held = store
        .for_resource(&scope, "room-1")
        .expect("a legacy record still parses");
    assert_eq!(held.len(), 2);
    let bo = held
        .iter()
        .find(|link| link.issued_to == "bo")
        .expect("bo's link folded");
    assert_eq!(bo.rotation_of, None);
    assert_eq!(bo.presentations, 0);
    assert_eq!(
        store
            .present(
                &scope,
                "room-1",
                "secret-b",
                &buyers(),
                now() + Duration::hours(1)
            )
            .expect("io"),
        Ok(admitted("bo", 1))
    );
}
