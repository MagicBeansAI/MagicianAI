//! The composition, as behaviour: real activity in, register rows out.
//!
//! Every test here SEEDS a store and asserts the seeded thing exists before
//! asserting anything about how it is filtered, refused or written. A test
//! over an empty store would pass against the very bug this module exists to
//! fix — the register that reads empty forever.

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::scheduling::{SchedulingScope, SchedulingStore, Slot};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::obligations::{
    ObligationDirection, ObligationScope, ObligationState, ObligationStore,
};
use magician::magician_v2::share_links::{IssueShareLink, ShareLinkScope, ShareLinkStore};
use magician_learning::data_room::{DataRoomScope, DataRoomStore, FollowUpPolicy, OpenDataRoom};

use super::worker::{finalize_tick, ObligationSweepConfig, ObligationSweepHealthSnapshot};
use super::{sweep_scope, SweepMemory, SweepPolicy};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn scope() -> ObligationScope {
    ObligationScope::new("anonymous", "default")
}

fn policy() -> SweepPolicy {
    SweepPolicy::new(
        Duration::days(3),
        FollowUpPolicy::new(Duration::days(2), Duration::days(5)).expect("follow-up policy"),
    )
    .expect("sweep policy")
}

fn workspace() -> (tempfile::TempDir, ArtifactV2Workspace) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let layout = ArtifactV2Workspace::new(tmp.path());
    (tmp, layout)
}

/// An ask offered `days_ago` and never answered.
fn seed_unanswered_ask(
    layout: &ArtifactV2Workspace,
    audience: &AudienceRef,
    counterparty: &str,
    days_ago: i64,
) {
    let store = SchedulingStore::new(layout.clone());
    let start = now() + Duration::days(7);
    let slot = Slot::new(start, start + Duration::hours(1)).expect("slot");
    store
        .open(
            &SchedulingScope::new("anonymous", "default"),
            audience,
            counterparty,
            "quarterly review",
            &[slot],
            Some("act-offer-1".to_string()),
            now() - Duration::days(days_ago),
        )
        .expect("open the ask");
}

/// A room shared `days_ago` with one identity who never opened it.
fn seed_never_opened_room(
    layout: &ArtifactV2Workspace,
    audience: &AudienceRef,
    issued_to: &str,
    days_ago: i64,
) -> String {
    let rooms = DataRoomStore::new(layout.clone());
    let room = rooms
        .open(
            &DataRoomScope::new("anonymous", "default"),
            &OpenDataRoom {
                audience: audience.clone(),
                opened_by: "owner".to_string(),
                closes_at: None,
            },
            now() - Duration::days(days_ago + 1),
        )
        .expect("open the room");
    ShareLinkStore::new(layout.clone())
        .issue(
            &ShareLinkScope::new("anonymous", "default"),
            &IssueShareLink {
                resource_ref: room.room_id.clone(),
                audience: audience.clone(),
                issued_to: issued_to.to_string(),
                secret: "a-secret".to_string(),
                expires_at: now() + Duration::days(30),
            },
            now() - Duration::days(days_ago),
        )
        .expect("issue the grant");
    room.room_id
}

/// Ripened silence on a scheduling ask reaches the register.
///
/// Pins the failure the whole module exists for: `sweep_silence` was complete
/// and called by nothing, so an offer nobody answered produced no row and the
/// register read empty — indistinguishable from a counterparty who had replied.
#[test]
fn an_unanswered_offer_becomes_a_chase_in_the_register() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::engagement("eng-1");
    seed_unanswered_ask(&layout, &audience, "dana", 4);

    // The ask exists before anything is asserted about the register.
    let asks = SchedulingStore::new(layout.clone())
        .all_negotiations(&SchedulingScope::new("anonymous", "default"))
        .expect("read the asks");
    assert_eq!(asks.len(), 1, "the seeded ask must be readable");

    let (report, _next) = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect("sweep");
    assert_eq!(report.negotiations_seen, 1);
    assert_eq!(report.recorded, 1);
    assert_eq!(report.settled, 0);

    let register = ObligationStore::new(layout.clone())
        .all_obligations(&scope())
        .expect("read the register");
    assert_eq!(register.len(), 1);
    assert_eq!(
        register[0].what,
        "a reply to the scheduling ask to dana about quarterly review"
    );
    assert_eq!(register[0].direction, ObligationDirection::OwedToUs);
    assert_eq!(register[0].created_by, "scheduling");
    assert_eq!(register[0].source_act_ref.as_deref(), Some("act-offer-1"));
    // `offered_at + window`, never the sweep's clock: a moving due date would
    // give every sweep a fresh identity tuple.
    assert_eq!(register[0].due_at, now() - Duration::days(1));
    assert_eq!(register[0].state(now()), ObligationState::Lapsed);
}

/// A share nobody opened becomes a delivery question in the register.
///
/// Pins the twin failure: `apply_follow_up_sweep` had no caller, so the room
/// shared with somebody who never opened it — the one signal that earns the
/// whole feature — wrote nothing anywhere.
#[test]
fn a_never_opened_share_becomes_a_delivery_question() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::account("acct-7");
    let room_id = seed_never_opened_room(&layout, &audience, "dana@example.test", 3);

    let rooms = DataRoomStore::new(layout.clone())
        .list(&DataRoomScope::new("anonymous", "default"))
        .expect("read the rooms");
    assert_eq!(rooms.len(), 1, "the seeded room must be readable");

    let (report, _next) = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect("sweep");
    assert_eq!(report.rooms_seen, 1);
    assert_eq!(report.recorded, 1);

    let register = ObligationStore::new(layout.clone())
        .all_obligations(&scope())
        .expect("read the register");
    assert_eq!(register.len(), 1);
    assert!(
        register[0].what.contains(&room_id),
        "the row must name the room by its stable id, not a label: {}",
        register[0].what
    );
    assert!(
        register[0].what.contains("dana@example.test"),
        "the row must name who it is about: {}",
        register[0].what
    );
    assert_eq!(register[0].direction, ObligationDirection::OwedByUs);
    assert_eq!(register[0].audience, audience);
}

/// A second sweep over unchanged activity resumes rows rather than doubling
/// them.
///
/// Pins the failure that makes a register unreadable: every tick minting a
/// fresh copy of the same chase, until the owner stops opening it.
#[test]
fn a_repeated_sweep_resumes_rather_than_duplicating() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::engagement("eng-1");
    seed_unanswered_ask(&layout, &audience, "dana", 4);

    let (first, memory) = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect("first sweep");
    assert_eq!(first.recorded, 1);
    let (second, _) = sweep_scope(
        &layout,
        &scope(),
        &memory,
        &policy(),
        now() + Duration::hours(1),
    )
    .expect("second sweep");
    assert_eq!(second.recorded, 1, "the row is resumed, so it is counted");

    let register = ObligationStore::new(layout.clone())
        .all_obligations(&scope())
        .expect("read the register");
    assert_eq!(
        register.len(),
        1,
        "two sweeps over one unanswered ask must leave one row"
    );
    assert_eq!(register[0].created_at, now(), "the first record stands");
}

/// The chase settles when they finally answer, and settles as MET.
///
/// Pins the pairing: `silence_follow_up` goes to `None` the instant a reply
/// lands, so without `silence_settlement` the row a previous sweep recorded
/// could never be closed and the register would grow forever. Met, not
/// released — a reply is exactly what was owed.
#[test]
fn an_answered_ask_settles_its_chase_as_met() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::engagement("eng-1");
    seed_unanswered_ask(&layout, &audience, "dana", 4);
    let (_, memory) = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect("first sweep");

    let scheduling = SchedulingScope::new("anonymous", "default");
    let store = SchedulingStore::new(layout.clone());
    let ask = store
        .all_negotiations(&scheduling)
        .expect("read")
        .pop()
        .expect("one ask");
    store
        .absorb(
            &scheduling,
            &audience,
            &ask.negotiation_id,
            &crate::scheduling::Reply {
                source_ref: "msg-1".to_string(),
                at: now(),
                kind: crate::scheduling::ReplyKind::Declined { reason: None },
            },
        )
        .expect("absorb the decline");

    let (report, _) = sweep_scope(
        &layout,
        &scope(),
        &memory,
        &policy(),
        now() + Duration::hours(1),
    )
    .expect("second sweep");
    assert_eq!(report.settled, 1);

    let register = ObligationStore::new(layout.clone())
        .all_obligations(&scope())
        .expect("read the register");
    assert_eq!(register.len(), 1);
    assert_eq!(
        register[0].state(now() + Duration::hours(1)),
        ObligationState::Met
    );
    assert!(!register[0].is_outstanding(now() + Duration::days(365)));
}

/// A first sweep settles nothing.
///
/// Pins the vacuous-emptiness bug: handing the derivations an EMPTY previous
/// view would say the last sweep saw no relationships, and the data room's
/// settle half — which derives from absence — would then release the whole
/// register in one call.
#[test]
fn a_first_sweep_settles_nothing() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::account("acct-7");
    seed_never_opened_room(&layout, &audience, "dana@example.test", 3);
    // Record the row, then sweep again as if this were a first run.
    let (seeded, _) = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect("seed the register");
    assert_eq!(seeded.recorded, 1);

    let (fresh, _) = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now() + Duration::hours(1),
    )
    .expect("a fresh first sweep");
    assert_eq!(
        fresh.settled, 0,
        "an unseeded memory must not read as a previous view that saw nothing"
    );

    let register = ObligationStore::new(layout.clone())
        .all_obligations(&scope())
        .expect("read the register");
    assert!(
        register[0].is_outstanding(now() + Duration::hours(1)),
        "the row must survive a first sweep untouched"
    );
}

/// A revoked grant still counts as a share nobody opened.
///
/// Pins the fail-open that filtering to live links would be: a link that
/// lapsed or was killed before anybody opened it is the purest form of "this
/// never reached them", and dropping it would delete exactly the row worth
/// worrying about.
#[test]
fn a_revoked_grant_still_raises_the_delivery_question() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::panel("panel-2");
    let room_id = seed_never_opened_room(&layout, &audience, "dana@example.test", 3);
    let killed = ShareLinkStore::new(layout.clone())
        .revoke(
            &ShareLinkScope::new("anonymous", "default"),
            &room_id,
            "dana@example.test",
            now() - Duration::days(1),
        )
        .expect("revoke");
    assert_eq!(killed.len(), 1, "the grant must exist to be revoked");
    assert!(killed[0].revoked_at.is_some());

    let (report, _) = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect("sweep");
    assert_eq!(
        report.recorded, 1,
        "a killed link that was never opened is still a delivery question"
    );
}

/// An unreadable log fails the sweep instead of reading as a quiet scope.
///
/// Pins the fail-open this codebase refuses everywhere else: a corrupted
/// register folded to "nothing is owed" would be the most reassuring wrong
/// answer the system could give.
#[test]
fn a_corrupt_log_fails_the_sweep_rather_than_reading_as_empty() {
    let (tmp, layout) = workspace();
    let audience = AudienceRef::engagement("eng-1");
    seed_unanswered_ask(&layout, &audience, "dana", 4);

    // Prove the sweep works before breaking the log.
    sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect("a healthy sweep");

    let dir = ArtifactV2Workspace::new(tmp.path())
        .scope_root("anonymous", "default")
        .join("scheduling");
    let log = std::fs::read_dir(&dir)
        .expect("the scheduling directory must exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("one negotiation log");
    let mut raw = std::fs::read_to_string(&log).expect("read the log");
    // A TERMINATED unparseable line is corruption, not a torn tail.
    raw.push_str("{not json}\n");
    std::fs::write(&log, raw).expect("write the log");

    let error = sweep_scope(
        &layout,
        &scope(),
        &SweepMemory::unseeded(),
        &policy(),
        now(),
    )
    .expect_err("a corrupt log must refuse");
    assert!(
        format!("{error:#}").contains("unparseable record"),
        "the refusal must name the corruption: {error:#}"
    );
}

/// A silence window at or below zero is refused, never substituted.
///
/// Pins the flood: at a zero window every offer is silent the instant it is
/// made, and the rows that would write into an append-only register cannot be
/// un-written.
#[test]
fn a_non_positive_silence_window_is_refused() {
    let follow_up = FollowUpPolicy::new(Duration::days(2), Duration::days(5)).expect("policy");
    for window in [Duration::zero(), Duration::seconds(-1)] {
        let error = SweepPolicy::new(window, follow_up)
            .expect_err("a non-positive silence window must refuse");
        assert!(
            format!("{error:#}").contains("must be positive"),
            "the refusal must say why: {error:#}"
        );
    }
    assert!(
        SweepPolicy::new(Duration::seconds(1), follow_up).is_ok(),
        "the smallest positive window is accepted, so the refusal is about sign not size"
    );
}

/// A tick that swept no scopes reports degraded, not idle.
///
/// Pins the green-dashboard failure: an empty workspace and an unlistable one
/// produce identical counts, and reading the vacuous case as success is how a
/// register stays empty for a year while every health probe is happy.
#[test]
fn a_tick_that_swept_no_scopes_is_degraded_not_idle() {
    let mut snapshot = ObligationSweepHealthSnapshot::configured(&ObligationSweepConfig::default());
    finalize_tick(&mut snapshot, "2026-08-20T12:00:00Z");
    assert_eq!(snapshot.state, "degraded");
    assert!(snapshot.last_error.is_some());
    assert!(
        snapshot.last_success_at.is_none(),
        "a tick that swept nothing must not stamp a success time"
    );

    let mut worked = ObligationSweepHealthSnapshot::configured(&ObligationSweepConfig::default());
    worked.scopes_seen = 2;
    finalize_tick(&mut worked, "2026-08-20T12:00:00Z");
    assert_eq!(worked.state, "idle", "a quiet day over real scopes is idle");
    assert_eq!(
        worked.last_success_at.as_deref(),
        Some("2026-08-20T12:00:00Z")
    );
}

/// A configured window that cannot be honoured refuses at build time.
///
/// Pins the substitution bug: a worker that started with a defaulted window
/// would write chases against a waiting period nobody chose.
#[test]
fn an_unusable_configured_window_refuses_the_policy() {
    let config = ObligationSweepConfig {
        delivery_question_after_hours: 0,
        ..ObligationSweepConfig::default()
    };
    assert!(
        config.policy().is_err(),
        "a zero delivery-question window must not be defaulted into a policy"
    );
    assert!(
        ObligationSweepConfig::default().policy().is_ok(),
        "the shipped defaults must build a usable policy"
    );
}
