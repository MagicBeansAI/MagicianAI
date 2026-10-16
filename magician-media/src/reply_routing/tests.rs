//! §3's non-skippable item 1, as behaviour: a reply must find its ask.

use chrono::{Duration, TimeZone, Utc};

use crate::scheduling::{absorb_reply, InboundReading, InboundReply, Slot};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::AudienceRef;

use super::*;

fn fixture() -> (tempfile::TempDir, SchedulingStore, SchedulingScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = SchedulingStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, SchedulingScope::new("anonymous", "default"))
}

fn at(days: i64) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 9, 0, 0).unwrap() + Duration::days(days)
}

fn acme() -> AudienceRef {
    AudienceRef::engagement("eng-acme")
}

fn slot(days: i64) -> Slot {
    Slot::new(at(days), at(days) + Duration::hours(1)).expect("well-formed slot")
}

fn ask(
    store: &SchedulingStore,
    scope: &SchedulingScope,
    audience: &AudienceRef,
    purpose: &str,
) -> String {
    store
        .open(scope, audience, "dana", purpose, &[slot(7)], None, at(0))
        .expect("ask")
        .negotiation_id
}

/// One open ask is the only case a caller may absorb against without choosing.
#[test]
fn one_open_ask_in_a_relationship_is_the_target() {
    let (_tmp, store, scope) = fixture();
    let id = ask(&store, &scope, &acme(), "the demo");

    let target = open_ask_for(&store, &scope, &acme()).expect("read");
    assert_eq!(target.reason(), "one_open_ask");
    assert_eq!(
        target.one().expect("one").negotiation_id,
        id,
        "the reply belongs to the ask that is waiting"
    );
}

/// Two open asks close NEITHER.
///
/// One reply answers one ask, and picking is a coin flip whose loser is settled
/// by words that were never about it. A counterparty with a demo AND a contract
/// review is legitimate, and the reply names which only in prose this module
/// deliberately does not read.
#[test]
fn two_open_asks_are_refused_by_name() {
    let (_tmp, store, scope) = fixture();
    let first = ask(&store, &scope, &acme(), "the demo");
    let second = ask(&store, &scope, &acme(), "the contract review");

    let target = open_ask_for(&store, &scope, &acme()).expect("read");
    assert!(target.one().is_none(), "a coin flip is not a target");
    assert_eq!(target.reason(), "ambiguous_open_asks");
    let ReplyTarget::Ambiguous { negotiation_ids } = target else {
        panic!("expected ambiguity");
    };
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(negotiation_ids, expected, "named, and in a stable order");
}

/// A relationship with nothing waiting is not an error.
///
/// A counterparty writes about many things. Treating every message that is not
/// a scheduling reply as a failure would make the loop unusable.
#[test]
fn a_relationship_with_no_open_ask_says_so_rather_than_failing() {
    let (_tmp, store, scope) = fixture();
    let target = open_ask_for(&store, &scope, &acme()).expect("read");
    assert!(matches!(target, ReplyTarget::NoOpenAsk));
    assert_eq!(target.reason(), "no_open_ask");
}

/// A settled ask absorbs nothing, so it is not a target — and a DECLINED one
/// still is.
///
/// Held and closed are the store's own settled states. A decline is an answer,
/// not an ending: the counterparty may still counter after it, and treating a
/// decline as settled would orphan exactly the reply that reopens the
/// conversation.
#[test]
fn held_and_closed_are_settled_but_declined_is_still_open() {
    let (_tmp, store, scope) = fixture();
    let id = ask(&store, &scope, &acme(), "the demo");
    store
        .close(&scope, &acme(), &id, "they went elsewhere", at(1))
        .expect("close");
    assert!(matches!(
        open_ask_for(&store, &scope, &acme()).expect("read"),
        ReplyTarget::NoOpenAsk
    ));

    // A fresh relationship, declined rather than closed.
    let globex = AudienceRef::engagement("eng-globex");
    let declined = ask(&store, &scope, &globex, "the intro call");
    // Through `absorb_reply`, not `SchedulingStore::absorb`: the store takes a
    // resolved `Reply`, and turning an inbound reading into one is the
    // consumer's job — including refusing an acceptance of a slot nobody
    // offered.
    absorb_reply(
        &store,
        &scope,
        &globex,
        &declined,
        &InboundReply {
            source_ref: "msg-1".to_string(),
            at: at(1),
            reading: InboundReading::Declined {
                reason: Some("that week is gone".to_string()),
            },
        },
    )
    .expect("decline");
    let target = open_ask_for(&store, &scope, &globex).expect("read");
    assert_eq!(
        target.one().expect("still open").negotiation_id,
        declined,
        "a decline is an answer, not an ending"
    );
}
