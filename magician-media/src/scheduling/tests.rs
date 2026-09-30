//! §3's Module A contract, as behaviour.

use chrono::{DateTime, Duration, TimeZone, Utc};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::obligations::ObligationDirection;

use super::store::{SchedulingScope, SchedulingStore};
use super::types::{Held, Negotiation, NegotiationState, Reply, ReplyKind, Reschedule, Slot};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn fixture() -> (tempfile::TempDir, SchedulingStore, SchedulingScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let scheduling = SchedulingStore::new(ArtifactV2Workspace::new(tmp.path()));
    (
        tmp,
        scheduling,
        SchedulingScope::new("anonymous", "default"),
    )
}

fn eng() -> AudienceRef {
    AudienceRef::engagement("eng-1")
}

/// A one-hour slot, `days` after the pinned clock, starting at `hour`.
fn slot(days: i64, hour: u32) -> Slot {
    let start = Utc.with_ymd_and_hms(2026, 8, 24, hour, 0, 0).unwrap() + Duration::days(days);
    Slot::new(start, start + Duration::hours(1)).expect("well-formed slot")
}

fn accepted(source_ref: &str, chosen: Slot) -> Reply {
    Reply {
        source_ref: source_ref.to_string(),
        at: now() + Duration::hours(4),
        kind: ReplyKind::Accepted { slot: chosen },
    }
}

fn countered(source_ref: &str, slots: Vec<Slot>) -> Reply {
    Reply {
        source_ref: source_ref.to_string(),
        at: now() + Duration::hours(4),
        kind: ReplyKind::Countered { slots },
    }
}

fn declined(source_ref: &str, reason: Option<&str>) -> Reply {
    Reply {
        source_ref: source_ref.to_string(),
        at: now() + Duration::hours(4),
        kind: ReplyKind::Declined {
            reason: reason.map(str::to_string),
        },
    }
}

/// One ask, opened with two candidate slots.
fn open_ask(scheduling: &SchedulingStore, scope: &SchedulingScope) -> Negotiation {
    scheduling
        .open(
            scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(1, 9), slot(2, 14)],
            Some("act-offer-1".to_string()),
            now(),
        )
        .expect("open")
}

/// *"id derived from (scope, audience, counterparty, purpose) so re-opening
/// the same ask resumes"* — an IDENTICAL replay (same slots, same offer act
/// ref) resumes instead of duplicating, and writes nothing.
#[test]
fn an_identical_open_replay_resumes_the_same_negotiation() {
    let (_tmp, scheduling, scope) = fixture();
    let first = open_ask(&scheduling, &scope);
    let again = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(1, 9), slot(2, 14)],
            Some("act-offer-1".to_string()),
            now() + Duration::hours(6),
        )
        .expect("replay");

    assert_eq!(again.negotiation_id, first.negotiation_id);
    assert_eq!(
        again, first,
        "a replay returns the standing record untouched"
    );
    assert_eq!(
        scheduling.for_audience(&scope, &eng()).expect("read"),
        vec![first],
        "one ask, however many times it is replayed"
    );
}

/// The failure this pins: `open` with different slots on a live round used to
/// return `Ok(existing)` while recording nothing — after a decline, the
/// genuinely new offer (and its act ref) landed nowhere, the caller believed
/// it stood, and the counterparty's acceptance of a new time was then refused
/// as an agreement that never happened, wedging the negotiation for good. A
/// changed open is now refused loudly, and the refusal names `re_offer`.
#[test]
fn a_changed_open_on_a_live_round_is_refused_and_names_re_offer() {
    let (_tmp, scheduling, scope) = fixture();
    let first = open_ask(&scheduling, &scope);

    let error = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(5, 11)],
            Some("act-offer-9".to_string()),
            now() + Duration::hours(6),
        )
        .expect_err("a changed offer must never be silently dropped");
    assert!(
        error.to_string().contains("re_offer"),
        "the refusal names the API that records a replacement, got: {error}"
    );
    let unchanged = scheduling
        .load(&scope, &eng(), &first.negotiation_id)
        .expect("load")
        .expect("present");
    assert_eq!(unchanged, first, "the mismatched open recorded nothing");

    // The exact wedge scenario: after their decline, new times through `open`
    // are refused (not swallowed) — the correction is `re_offer`'s job.
    scheduling
        .absorb(
            &scope,
            &eng(),
            &first.negotiation_id,
            &declined("msg-1", None),
        )
        .expect("decline");
    let after_decline = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(3, 10), slot(4, 15)],
            Some("act-offer-2".to_string()),
            now() + Duration::days(1),
        )
        .expect_err("new times after a decline are a re_offer, never a silent no-op");
    assert!(after_decline.to_string().contains("re_offer"));

    // An identical replay of the ORIGINAL open still resumes, whatever state
    // the round has since reached — a retried call must stay safe.
    let replay = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(1, 9), slot(2, 14)],
            Some("act-offer-1".to_string()),
            now() + Duration::days(1),
        )
        .expect("identical replay");
    assert_eq!(replay.state(), NegotiationState::Declined);
    assert_eq!(replay.replies.len(), 1);
}

/// The identity tuple: a different purpose or counterparty is a different ask,
/// and the audience KIND is part of the key — `engagement:eng-1` and
/// `account:eng-1` never merge, because the id uses `as_key()`, never the id
/// alone.
#[test]
fn the_identity_tuple_separates_counterparty_purpose_and_kind() {
    let (_tmp, scheduling, scope) = fixture();
    let base = open_ask(&scheduling, &scope);
    let other_purpose = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "contract renewal",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("other purpose");
    let other_counterparty = scheduling
        .open(
            &scope,
            &eng(),
            "mira",
            "quarterly review",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("other counterparty");
    let other_kind = scheduling
        .open(
            &scope,
            &AudienceRef::account("eng-1"),
            "dana",
            "quarterly review",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("other kind");

    assert_ne!(base.negotiation_id, other_purpose.negotiation_id);
    assert_ne!(base.negotiation_id, other_counterparty.negotiation_id);
    assert_ne!(
        base.negotiation_id, other_kind.negotiation_id,
        "the same id under a different audience kind is a different relationship"
    );
    assert_eq!(
        scheduling.for_audience(&scope, &eng()).expect("read").len(),
        3
    );
    assert_eq!(
        scheduling
            .for_audience(&scope, &AudienceRef::account("eng-1"))
            .expect("read")
            .len(),
        1
    );
}

/// *"an Accepted slot MUST be one of the currently-standing slots… accepting a
/// slot nobody offered is recording an agreement that never happened"*.
#[test]
fn accepting_a_slot_nobody_offered_is_refused() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);

    let refused = scheduling.absorb(
        &scope,
        &eng(),
        &opened.negotiation_id,
        &accepted("msg-1", slot(9, 16)),
    );
    assert!(refused.is_err());

    let unchanged = scheduling
        .load(&scope, &eng(), &opened.negotiation_id)
        .expect("load")
        .expect("present");
    assert_eq!(
        unchanged.replies,
        Vec::new(),
        "the phantom agreement left no trace"
    );
    assert_eq!(unchanged.state(), NegotiationState::AwaitingReply);
}

/// *"A Countered reply REPLACES the standing slots"* — after a counter the
/// original offer is no longer standing, so an acceptance of an original slot
/// is refused while an acceptance of a countered slot lands.
#[test]
fn a_counter_replaces_the_standing_slots() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let their_slot = slot(4, 10);

    let after_counter = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &countered("msg-1", vec![their_slot]),
        )
        .expect("counter");
    assert_eq!(after_counter.state(), NegotiationState::Countered);
    assert_eq!(after_counter.standing_slots(), &[their_slot][..]);
    assert_eq!(
        after_counter.offered,
        vec![slot(1, 9), slot(2, 14)],
        "the offer stays in the record; it just no longer stands"
    );

    let original_after_counter = scheduling.absorb(
        &scope,
        &eng(),
        &opened.negotiation_id,
        &accepted("msg-2", slot(1, 9)),
    );
    assert!(
        original_after_counter.is_err(),
        "the original offer is no longer standing once they counter"
    );

    let agreed = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-3", their_slot),
        )
        .expect("accept the counter");
    assert_eq!(agreed.state(), NegotiationState::Accepted);
    assert_eq!(agreed.accepted_slot(), Some(their_slot));
}

/// `absorb(reply) — … decline …`: a decline is recorded with their words'
/// source, so the owner can read what was actually said.
#[test]
fn a_decline_is_recorded_in_their_words() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);

    let reply = declined("msg-1", Some("travelling that whole week"));
    let after = scheduling
        .absorb(&scope, &eng(), &opened.negotiation_id, &reply)
        .expect("decline");

    assert_eq!(after.state(), NegotiationState::Declined);
    assert_eq!(after.replies, vec![reply]);
}

/// *"idempotent on source_ref (a re-read inbox must not double-record)"* —
/// and the dedupe is by source, not by content: a second line claiming the
/// same source cannot rewrite what the first said.
#[test]
fn absorb_is_idempotent_on_source_ref() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let reply = accepted("msg-1", slot(1, 9));

    let first = scheduling
        .absorb(&scope, &eng(), &opened.negotiation_id, &reply)
        .expect("first");
    let replay = scheduling
        .absorb(&scope, &eng(), &opened.negotiation_id, &reply)
        .expect("replay");
    assert_eq!(replay, first);

    let same_source_new_words = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &declined("msg-1", None),
        )
        .expect("same source again");
    assert_eq!(
        same_source_new_words.replies,
        vec![reply],
        "the first absorption of a source is the absorption"
    );
    assert_eq!(same_source_new_words.state(), NegotiationState::Accepted);
}

/// *"refuse absorb after Held or Closed (the negotiation is settled)"* —
/// though a replay of an already-recorded source still returns cleanly,
/// because a re-read inbox is not a new reply.
#[test]
fn a_settled_negotiation_absorbs_nothing() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let chosen = slot(1, 9);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("accept");
    scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-1",
            now() + Duration::hours(5),
        )
        .expect("hold");

    assert!(
        scheduling
            .absorb(
                &scope,
                &eng(),
                &opened.negotiation_id,
                &countered("msg-2", vec![slot(3, 9)])
            )
            .is_err(),
        "a reply after the hold is a reschedule conversation, not part of this record"
    );
    let replay = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("replaying a recorded source is not a new absorb");
    assert_eq!(replay.replies.len(), 1);

    let other = scheduling
        .open(
            &scope,
            &eng(),
            "mira",
            "kickoff",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("open");
    scheduling
        .close(
            &scope,
            &eng(),
            &other.negotiation_id,
            "they went with someone else",
            now(),
        )
        .expect("close");
    assert!(scheduling
        .absorb(
            &scope,
            &eng(),
            &other.negotiation_id,
            &declined("msg-3", None)
        )
        .is_err());
}

/// `hold(slot)` requires an accepted negotiation and the accepted slot:
/// holding before they accept books a time nobody agreed to, and holding a
/// different time records an agreement that never happened.
#[test]
fn hold_requires_an_accepted_negotiation_and_the_accepted_slot() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let chosen = slot(1, 9);

    assert!(
        scheduling
            .hold(
                &scope,
                &eng(),
                &opened.negotiation_id,
                chosen,
                "cal-1",
                now()
            )
            .is_err(),
        "no acceptance yet, so nothing may be booked"
    );

    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("accept");

    assert!(
        scheduling
            .hold(
                &scope,
                &eng(),
                &opened.negotiation_id,
                slot(2, 14),
                "cal-1",
                now()
            )
            .is_err(),
        "the other offered slot is not the one they accepted"
    );
    assert!(
        scheduling
            .hold(&scope, &eng(), &opened.negotiation_id, chosen, "   ", now())
            .is_err(),
        "a hold with no calendar event ref points at nothing"
    );

    let held_at = now() + Duration::hours(5);
    let held = scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-1",
            held_at,
        )
        .expect("hold");
    assert_eq!(held.state(), NegotiationState::Held);
    assert_eq!(
        held.held,
        Some(Held {
            slot: chosen,
            calendar_event_ref: "cal-1".to_string(),
            at: held_at,
        })
    );
}

/// The first hold is the hold: an identical retry resumes, a conflicting hold
/// is refused rather than orphaning the booked event.
#[test]
fn a_hold_retry_resumes_and_a_conflicting_hold_is_refused() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let chosen = slot(1, 9);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("accept");
    let first = scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-1",
            now() + Duration::hours(5),
        )
        .expect("hold");

    let retry = scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-1",
            now() + Duration::hours(8),
        )
        .expect("retry");
    assert_eq!(
        retry, first,
        "the retry resumes; the booking's time does not move"
    );

    assert!(scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-2",
            now()
        )
        .is_err());
}

/// `reschedule(event, why)`: only from Held; *"clears held… returns to needing
/// a fresh offer (offered cleared — the old times are dead); keep full
/// history"*. The same ask then resumes with a fresh offer under the same id.
#[test]
fn reschedule_only_from_held_resets_the_round_and_keeps_history() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    assert!(
        scheduling
            .reschedule(
                &scope,
                &eng(),
                &opened.negotiation_id,
                "they asked to move it",
                now()
            )
            .is_err(),
        "before a hold there is no booked event to move"
    );

    let chosen = slot(1, 9);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("accept");
    let held_at = now() + Duration::hours(5);
    scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-1",
            held_at,
        )
        .expect("hold");

    let moved_at = now() + Duration::days(1);
    let moved = scheduling
        .reschedule(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "they asked to move it",
            moved_at,
        )
        .expect("reschedule");
    assert_eq!(moved.held, None);
    assert_eq!(moved.offered, Vec::new(), "the old times are dead");
    assert_eq!(
        moved.replies,
        Vec::new(),
        "the round reset; the log keeps the words"
    );
    assert_eq!(moved.state(), NegotiationState::AwaitingReply);
    assert_eq!(
        moved.reschedules,
        vec![Reschedule {
            at: moved_at,
            why: "they asked to move it".to_string(),
            released: Held {
                slot: chosen,
                calendar_event_ref: "cal-1".to_string(),
                at: held_at,
            },
        }],
        "the history keeps the booking that died, and why"
    );
    assert_eq!(
        moved.silent_since(Duration::hours(48), now() + Duration::days(30)),
        None,
        "with no standing offer there is nothing to be silent about"
    );

    let fresh_at = now() + Duration::days(2);
    let fresh = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(10, 15)],
            Some("act-offer-2".to_string()),
            fresh_at,
        )
        .expect("fresh offer");
    assert_eq!(
        fresh.negotiation_id, opened.negotiation_id,
        "the same ask resumes"
    );
    assert_eq!(fresh.offered, vec![slot(10, 15)]);
    assert_eq!(fresh.offered_at, fresh_at);
    assert_eq!(fresh.offer_act_ref, Some("act-offer-2".to_string()));
    assert_eq!(
        fresh.reschedules.len(),
        1,
        "the history survives the fresh offer"
    );
}

/// Silence ripens at exactly `offered_at + window`, inclusive — and the
/// ripening instant is stable, not the sweep's clock.
#[test]
fn silence_ripens_exactly_at_the_window_inclusive() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let window = Duration::hours(48);
    let ripens = now() + window;

    assert_eq!(
        opened.silent_since(window, ripens - Duration::seconds(1)),
        None
    );
    assert_eq!(
        opened.silent_since(window, ripens),
        Some(ripens),
        "inclusive at the boundary"
    );
    assert_eq!(
        opened.silent_since(window, ripens + Duration::days(3)),
        Some(ripens),
        "a later sweep sees the same ripening instant, not its own clock"
    );
}

/// *"Silence is a state. No answer is neither yes nor no"* — and one answer of
/// any kind ends it, as does settling the negotiation.
#[test]
fn silence_vanishes_the_moment_any_reply_lands() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let window = Duration::hours(48);
    let long_after = now() + Duration::days(10);

    let answered = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &declined("msg-1", None),
        )
        .expect("decline");
    assert_eq!(answered.silent_since(window, long_after), None);
    assert!(answered.silence_follow_up(window, long_after).is_none());

    let other = scheduling
        .open(
            &scope,
            &eng(),
            "mira",
            "kickoff",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("open");
    let closed = scheduling
        .close(
            &scope,
            &eng(),
            &other.negotiation_id,
            "overtaken by events",
            now(),
        )
        .expect("close");
    assert_eq!(
        closed.silent_since(window, long_after),
        None,
        "a settled ask is not silent"
    );
}

/// The Module D hand-over: *"a thing to chase later, which is Module D"*. The
/// obligation tuple is sweep-stable — `due_at` is the ripening instant, never
/// the sweep's `now`, because a moving due date floods the register.
#[test]
fn the_silence_follow_up_is_sweep_stable() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let window = Duration::hours(48);
    let ripens = now() + window;

    let first_sweep = opened.silence_follow_up(window, ripens).expect("ripened");
    let later_sweep = opened
        .silence_follow_up(window, ripens + Duration::days(4))
        .expect("still ripened");

    assert_eq!(first_sweep.due_at, ripens);
    assert_eq!(later_sweep.due_at, first_sweep.due_at);
    assert_eq!(
        first_sweep.what,
        "a reply to the scheduling ask to dana about quarterly review"
    );
    assert_eq!(later_sweep.what, first_sweep.what);
    assert_eq!(first_sweep.direction, ObligationDirection::OwedToUs);
    assert!(
        !first_sweep.direction.lapse_is_ours(),
        "they owe us the answer"
    );
    assert_eq!(first_sweep.audience, eng());
    assert_eq!(first_sweep.source_act_ref, Some("act-offer-1".to_string()));
    assert_eq!(later_sweep.source_act_ref, first_sweep.source_act_ref);
    assert_eq!(first_sweep.created_by, later_sweep.created_by);
    assert_eq!(first_sweep.program_id, None);
}

/// Fail closed: inverted or zero-length slots, empty offers, blank names and
/// non-positive silence windows are refusals, never permissive defaults.
#[test]
fn malformed_inputs_are_refused() {
    let (_tmp, scheduling, scope) = fixture();
    let start = now() + Duration::days(1);
    assert!(
        Slot::new(start, start).is_err(),
        "a zero-length slot cannot be met in"
    );
    assert!(Slot::new(start, start - Duration::hours(1)).is_err());

    assert!(scheduling
        .open(&scope, &eng(), "dana", "quarterly review", &[], None, now())
        .is_err());
    assert!(scheduling
        .open(
            &scope,
            &eng(),
            "   ",
            "quarterly review",
            &[slot(1, 9)],
            None,
            now()
        )
        .is_err());
    assert!(scheduling
        .open(&scope, &eng(), "dana", "   ", &[slot(1, 9)], None, now())
        .is_err());
    assert!(scheduling
        .open(
            &scope,
            &AudienceRef::engagement("  "),
            "dana",
            "quarterly review",
            &[slot(1, 9)],
            None,
            now()
        )
        .is_err());
    // A slot that arrived through deserialisation rather than `Slot::new` is
    // re-checked at the door.
    let inverted = Slot {
        start,
        end: start - Duration::hours(1),
    };
    assert!(scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[inverted],
            None,
            now()
        )
        .is_err());

    let opened = open_ask(&scheduling, &scope);
    let mut unsourced = declined("x", None);
    unsourced.source_ref = "   ".to_string();
    assert!(
        scheduling
            .absorb(&scope, &eng(), &opened.negotiation_id, &unsourced)
            .is_err(),
        "a reply with no source cannot be checked against the words"
    );
    assert!(scheduling
        .close(&scope, &eng(), &opened.negotiation_id, "   ", now())
        .is_err());

    let long_after = now() + Duration::days(30);
    assert_eq!(opened.silent_since(Duration::zero(), long_after), None);
    assert_eq!(opened.silent_since(Duration::hours(-2), long_after), None);
    assert!(opened
        .silence_follow_up(Duration::zero(), long_after)
        .is_none());
}

/// The vacuous-truth bug class, on the counter: a counter naming no slots
/// would replace the standing offer with nothing while reading as a live
/// counter, so it is refused.
#[test]
fn a_counter_naming_no_slots_is_refused() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);

    assert!(scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &countered("msg-1", Vec::new())
        )
        .is_err());

    let unchanged = scheduling
        .load(&scope, &eng(), &opened.negotiation_id)
        .expect("load")
        .expect("present");
    assert_eq!(unchanged.standing_slots(), &[slot(1, 9), slot(2, 14)][..]);
    assert_eq!(unchanged.replies, Vec::new());
}

/// Negotiations do not leak across scopes or audiences.
#[test]
fn negotiations_do_not_leak_across_scopes_or_audiences() {
    let (_tmp, scheduling, scope) = fixture();
    open_ask(&scheduling, &scope);

    assert_eq!(
        scheduling
            .for_audience(&scope, &AudienceRef::engagement("eng-2"))
            .expect("other engagement"),
        Vec::new()
    );
    assert_eq!(
        scheduling
            .for_audience(&scope, &AudienceRef::account("eng-1"))
            .expect("same id, different kind"),
        Vec::new()
    );
    assert_eq!(
        scheduling
            .for_audience(&SchedulingScope::new("someone-else", "default"), &eng())
            .expect("other scope"),
        Vec::new()
    );
}

/// The derived state covers every arm, and each arm's token matches its serde
/// name.
#[test]
fn state_derivation_covers_every_arm() {
    let (_tmp, scheduling, scope) = fixture();

    let awaiting = open_ask(&scheduling, &scope);
    assert_eq!(awaiting.state(), NegotiationState::AwaitingReply);
    assert_eq!(awaiting.state().as_str(), "awaiting_reply");

    let chosen = slot(1, 9);
    let accepted_state = scheduling
        .absorb(
            &scope,
            &eng(),
            &awaiting.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("accept");
    assert_eq!(accepted_state.state(), NegotiationState::Accepted);
    assert_eq!(accepted_state.state().as_str(), "accepted");

    let held_state = scheduling
        .hold(
            &scope,
            &eng(),
            &awaiting.negotiation_id,
            chosen,
            "cal-1",
            now(),
        )
        .expect("hold");
    assert_eq!(held_state.state(), NegotiationState::Held);
    assert_eq!(held_state.state().as_str(), "held");
    assert!(held_state.state().is_settled());

    let declined_ask = scheduling
        .open(
            &scope,
            &eng(),
            "mira",
            "kickoff",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("open");
    let declined_state = scheduling
        .absorb(
            &scope,
            &eng(),
            &declined_ask.negotiation_id,
            &declined("msg-2", None),
        )
        .expect("decline");
    assert_eq!(declined_state.state(), NegotiationState::Declined);
    assert_eq!(declined_state.state().as_str(), "declined");

    let countered_ask = scheduling
        .open(
            &scope,
            &eng(),
            "ravi",
            "pricing",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("open");
    let countered_state = scheduling
        .absorb(
            &scope,
            &eng(),
            &countered_ask.negotiation_id,
            &countered("msg-3", vec![slot(6, 9)]),
        )
        .expect("counter");
    assert_eq!(countered_state.state(), NegotiationState::Countered);
    assert_eq!(countered_state.state().as_str(), "countered");

    let closed_state = scheduling
        .close(
            &scope,
            &eng(),
            &countered_ask.negotiation_id,
            "overtaken by events",
            now(),
        )
        .expect("close");
    assert_eq!(closed_state.state(), NegotiationState::Closed);
    assert_eq!(closed_state.state().as_str(), "closed");
    assert!(closed_state.state().is_settled());
}

/// `close` is idempotent: the first close is the close, and a later call
/// cannot move the date or reword the reason.
#[test]
fn close_is_idempotent_and_the_first_reason_stands() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);

    let first = scheduling
        .close(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "they went quiet for a month",
            now(),
        )
        .expect("first close");
    let again = scheduling
        .close(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "changed my mind",
            now() + Duration::days(2),
        )
        .expect("second close");

    assert_eq!(again, first, "the first close stands");
    assert_eq!(
        again.closed.as_ref().map(|closed| closed.reason.as_str()),
        Some("they went quiet for a month")
    );
    assert_eq!(again.closed.as_ref().map(|closed| closed.at), Some(now()));
}

/// The one log file the fixture's store has written, found by walking the
/// temp root — the store's path derivation is private, and these tests need
/// the raw bytes to assert what the log holds and to injure it.
fn only_log_file(root: &std::path::Path) -> std::path::PathBuf {
    fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    assert_eq!(found.len(), 1, "exactly one scheduling log expected");
    found.remove(0)
}

/// The wedge this module shipped with: after their decline, `open` silently
/// swallowed a genuinely new offer (returning `Ok` while recording nothing),
/// so the counterparty's acceptance of a new time was refused as "an
/// agreement that never happened" — and no API could recover. `re_offer`
/// records the replacement: the round resets around the new slots, silence is
/// derivable from the NEW offer, and the acceptance lands all the way to a
/// hold.
#[test]
fn re_offer_after_a_decline_replaces_the_offer_and_the_acceptance_lands() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &declined("msg-1", Some("neither works")),
        )
        .expect("decline");

    let re_offered_at = now() + Duration::days(1);
    let new_slots = [slot(3, 10), slot(4, 15)];
    let re_offered = scheduling
        .re_offer(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &new_slots,
            Some("act-offer-2".to_string()),
            re_offered_at,
        )
        .expect("re-offer");
    assert_eq!(re_offered.state(), NegotiationState::AwaitingReply);
    assert_eq!(re_offered.offered, new_slots.to_vec());
    assert_eq!(re_offered.offered_at, re_offered_at);
    assert_eq!(re_offered.offer_act_ref, Some("act-offer-2".to_string()));
    assert_eq!(
        re_offered.replies,
        Vec::new(),
        "the round reset; the log keeps the decline"
    );

    let window = Duration::hours(48);
    assert_eq!(
        re_offered.silent_since(window, re_offered_at + window),
        Some(re_offered_at + window),
        "silence over the SECOND offer ripens from its own offered_at — before \
         the fix the dropped re-offer meant no second silence was ever derivable"
    );

    let agreed = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-2", slot(3, 10)),
        )
        .expect("accepting a re-offered time is an agreement that DID happen");
    assert_eq!(agreed.state(), NegotiationState::Accepted);
    assert_eq!(agreed.accepted_slot(), Some(slot(3, 10)));

    let held = scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            slot(3, 10),
            "cal-1",
            now() + Duration::days(2),
        )
        .expect("hold the re-offered time");
    assert_eq!(held.state(), NegotiationState::Held);
}

/// A re-offer after their counter is us countering back: it REPLACES the
/// standing slots, so neither their countered time nor our original offer can
/// be "accepted" afterwards — only the re-offered times stand.
#[test]
fn re_offer_after_a_counter_replaces_the_standing_slots() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let their_slot = slot(4, 10);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &countered("msg-1", vec![their_slot]),
        )
        .expect("counter");

    let ours = slot(6, 11);
    let re_offered = scheduling
        .re_offer(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &[ours],
            Some("act-offer-2".to_string()),
            now() + Duration::days(1),
        )
        .expect("counter their counter");
    assert_eq!(re_offered.state(), NegotiationState::AwaitingReply);
    assert_eq!(re_offered.standing_slots(), &[ours][..]);

    assert!(
        scheduling
            .absorb(
                &scope,
                &eng(),
                &opened.negotiation_id,
                &accepted("msg-2", their_slot)
            )
            .is_err(),
        "their countered time died when we re-offered"
    );
    assert!(
        scheduling
            .absorb(
                &scope,
                &eng(),
                &opened.negotiation_id,
                &accepted("msg-3", slot(1, 9))
            )
            .is_err(),
        "the original offer died when they countered"
    );
    let agreed = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-4", ours),
        )
        .expect("accept the re-offer");
    assert_eq!(agreed.accepted_slot(), Some(ours));
}

/// `re_offer` answers a decline or a counter (or fills the empty round a
/// reschedule leaves). From AwaitingReply there is no reply to answer; from
/// Accepted it would discard an agreement; from Held and Closed the round is
/// settled. Each refusal leaves the record untouched, and a malformed
/// re-offer is refused even from an answerable state.
#[test]
fn re_offer_is_refused_unless_it_answers_a_reply_or_fills_an_empty_round() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);

    assert!(
        scheduling
            .re_offer(
                &scope,
                &eng(),
                &opened.negotiation_id,
                &[slot(3, 10)],
                None,
                now()
            )
            .is_err(),
        "awaiting their reply: there is nothing to answer yet"
    );

    let chosen = slot(1, 9);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("accept");
    assert!(
        scheduling
            .re_offer(
                &scope,
                &eng(),
                &opened.negotiation_id,
                &[slot(3, 10)],
                None,
                now()
            )
            .is_err(),
        "accepted: replacing the offer would discard an agreement that did happen"
    );

    scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-1",
            now(),
        )
        .expect("hold");
    assert!(
        scheduling
            .re_offer(
                &scope,
                &eng(),
                &opened.negotiation_id,
                &[slot(3, 10)],
                None,
                now()
            )
            .is_err(),
        "held: new times over a booked event are a reschedule"
    );
    let unchanged = scheduling
        .load(&scope, &eng(), &opened.negotiation_id)
        .expect("load")
        .expect("present");
    assert_eq!(unchanged.state(), NegotiationState::Held);
    assert_eq!(
        unchanged.offered,
        vec![slot(1, 9), slot(2, 14)],
        "every refusal wrote nothing"
    );

    let closed_ask = scheduling
        .open(
            &scope,
            &eng(),
            "mira",
            "kickoff",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("open");
    scheduling
        .close(
            &scope,
            &eng(),
            &closed_ask.negotiation_id,
            "overtaken by events",
            now(),
        )
        .expect("close");
    assert!(
        scheduling
            .re_offer(
                &scope,
                &eng(),
                &closed_ask.negotiation_id,
                &[slot(3, 10)],
                None,
                now()
            )
            .is_err(),
        "closed: the round ended, the next round is an open"
    );

    let answerable = scheduling
        .open(
            &scope,
            &eng(),
            "ravi",
            "pricing",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("open");
    scheduling
        .absorb(
            &scope,
            &eng(),
            &answerable.negotiation_id,
            &declined("msg-2", None),
        )
        .expect("decline");
    assert!(
        scheduling
            .re_offer(&scope, &eng(), &answerable.negotiation_id, &[], None, now())
            .is_err(),
        "a re-offer of nothing gives them nothing to accept"
    );
    let inverted = Slot {
        start: now() + Duration::days(1),
        end: now() + Duration::days(1) - Duration::hours(1),
    };
    assert!(
        scheduling
            .re_offer(
                &scope,
                &eng(),
                &answerable.negotiation_id,
                &[inverted],
                None,
                now()
            )
            .is_err(),
        "a time nobody can attend cannot be offered"
    );
    let still_declined = scheduling
        .load(&scope, &eng(), &answerable.negotiation_id)
        .expect("load")
        .expect("present");
    assert_eq!(still_declined.state(), NegotiationState::Declined);
    assert_eq!(still_declined.offered, vec![slot(1, 9)]);
}

/// The failure this pins: a closed round was a permanent tombstone — `open`
/// on a recurring ask returned the closed record while recording nothing, so
/// the next quarter's offer never existed, no silence could ever ripen, and
/// nothing could ever reopen or re-key the ask. A fresh open now starts a NEW
/// round in the same identity slot, with every earlier round intact in the
/// log.
#[test]
fn reopening_a_closed_ask_starts_a_fresh_round_with_history_intact() {
    let (tmp, scheduling, scope) = fixture();
    let q3 = open_ask(&scheduling, &scope);
    scheduling
        .close(
            &scope,
            &eng(),
            &q3.negotiation_id,
            "met on the 24th; done",
            now(),
        )
        .expect("close");

    let q4_at = now() + Duration::days(90);
    let q4 = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(90, 9)],
            Some("act-offer-q4".to_string()),
            q4_at,
        )
        .expect("the recurring ask negotiates again");
    assert_eq!(
        q4.negotiation_id, q3.negotiation_id,
        "same identity slot across rounds"
    );
    assert_eq!(q4.closed, None);
    assert_eq!(q4.offered, vec![slot(90, 9)]);
    assert_eq!(q4.offered_at, q4_at);
    assert_eq!(q4.offer_act_ref, Some("act-offer-q4".to_string()));
    assert_eq!(q4.replies, Vec::new());
    assert_eq!(q4.reschedules, Vec::new());
    assert_eq!(q4.state(), NegotiationState::AwaitingReply);

    let window = Duration::hours(48);
    assert_eq!(
        q4.silent_since(window, q4_at + window),
        Some(q4_at + window),
        "the new round's silence can ripen — the tombstone kept this at None forever"
    );

    // The fold shows the current round; the log keeps every round.
    assert_eq!(
        scheduling.for_audience(&scope, &eng()).expect("read"),
        vec![q4.clone()]
    );
    let raw = std::fs::read_to_string(only_log_file(tmp.path())).expect("raw log");
    assert_eq!(
        raw.matches("\"record\":\"opened\"").count(),
        2,
        "both rounds' opens survive in the log"
    );
    assert_eq!(
        raw.matches("\"record\":\"closed\"").count(),
        1,
        "the first round's close survives in the log"
    );

    // The new round settles on its own terms, not the old round's close.
    let q4_closed = scheduling
        .close(
            &scope,
            &eng(),
            &q4.negotiation_id,
            "met early",
            q4_at + Duration::days(1),
        )
        .expect("close the new round");
    assert_eq!(
        q4_closed
            .closed
            .as_ref()
            .map(|closed| closed.reason.as_str()),
        Some("met early")
    );
    assert_eq!(
        q4_closed.closed.as_ref().map(|closed| closed.at),
        Some(q4_at + Duration::days(1))
    );
}

/// Every mutation now folds the log once and returns the appended record
/// applied to that fold in memory (instead of re-reading the whole file a
/// second time). This pins the equivalence that makes the refactor safe: the
/// state a mutation returns is exactly the state the next read folds, for
/// every mutation and both of `open`'s write paths.
#[test]
fn a_mutations_returned_state_is_exactly_what_the_next_read_folds() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let reload = || {
        scheduling
            .load(&scope, &eng(), &opened.negotiation_id)
            .expect("load")
            .expect("present")
    };

    let countered_state = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &countered("msg-1", vec![slot(4, 10)]),
        )
        .expect("counter");
    assert_eq!(countered_state, reload());

    let ours = slot(6, 11);
    let re_offered = scheduling
        .re_offer(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &[ours],
            Some("act-2".to_string()),
            now() + Duration::hours(1),
        )
        .expect("re-offer");
    assert_eq!(re_offered, reload());

    let accepted_state = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-2", ours),
        )
        .expect("accept");
    assert_eq!(accepted_state, reload());

    let held_state = scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            ours,
            "cal-1",
            now() + Duration::hours(6),
        )
        .expect("hold");
    assert_eq!(held_state, reload());

    let moved = scheduling
        .reschedule(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "they asked to move it",
            now() + Duration::days(1),
        )
        .expect("reschedule");
    assert_eq!(moved, reload());

    let fresh = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(8, 9)],
            Some("act-3".to_string()),
            now() + Duration::days(2),
        )
        .expect("fresh offer after the reschedule");
    assert_eq!(fresh, reload());

    let closed_state = scheduling
        .close(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "wrapped up",
            now() + Duration::days(3),
        )
        .expect("close");
    assert_eq!(closed_state, reload());
}

/// The failure this pins: a crash mid-append leaves one torn line at the
/// tail, and before this fix that fragment errored EVERY later read and write
/// for the whole audience, forever — one crash poisoned every negotiation in
/// the relationship. A torn tail is an append that never happened; everything
/// before it folds.
#[test]
fn a_torn_tail_is_an_append_that_never_happened_not_a_poisoned_log() {
    let (tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &declined("msg-1", None),
        )
        .expect("decline");

    let log = only_log_file(tmp.path());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .expect("open log");
    std::io::Write::write_all(
        &mut file,
        b"{\"record\":\"replied\",\"negotiation_id\":\"neg-",
    )
    .expect("tear the tail");

    let folded = scheduling
        .for_audience(&scope, &eng())
        .expect("a torn tail is not corruption");
    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0].state(), NegotiationState::Declined);
    assert_eq!(folded[0].replies.len(), 1, "the torn reply never happened");
}

/// The failure this pins: every read error — not just absence — folded to an
/// empty store, so a permission fault or corrupted log made `for_audience`
/// confidently answer "nothing" and let `open` fork a duplicate ask over a
/// negotiation that was already booked. Unreadable is an error the caller
/// sees, never an absence the caller trusts.
#[test]
fn an_unreadable_log_is_an_error_never_an_empty_store() {
    let (tmp, scheduling, scope) = fixture();
    open_ask(&scheduling, &scope);

    let log = only_log_file(tmp.path());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .expect("open log");
    std::io::Write::write_all(&mut file, &[0xFF, 0xFE, b'\n']).expect("poison with non-UTF8");

    let error = scheduling
        .for_audience(&scope, &eng())
        .expect_err("unreadable must not read as empty");
    assert!(
        error
            .to_string()
            .contains("an unreadable log must never be treated as an empty one"),
        "got: {error}"
    );
    assert!(
        scheduling
            .open(
                &scope,
                &eng(),
                "dana",
                "quarterly review",
                &[slot(1, 9)],
                Some("act-offer-1".to_string()),
                now(),
            )
            .is_err(),
        "an open over an unreadable log must not fork a duplicate ask"
    );
}

/// The failure this pins: `open`'s post-reschedule fill path guarded only on
/// an empty offer and no hold — `replies` was not checked — so after a
/// reschedule followed by the counterparty's absorbed counter, a retried
/// `open` wrote an `Offered` record over the answered round: our slots landed
/// under their standing counter as an offer that never actually stood, and
/// `offered_at` restarted a silence clock over it. With their counter
/// standing, new times from us are an answer to it — the open now falls
/// through to the refusal naming `re_offer`, and the record keeps their
/// counter untouched.
#[test]
fn open_cannot_overwrite_a_counter_standing_in_a_rescheduled_round() {
    let (tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let chosen = slot(1, 9);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &accepted("msg-1", chosen),
        )
        .expect("accept");
    scheduling
        .hold(
            &scope,
            &eng(),
            &opened.negotiation_id,
            chosen,
            "cal-1",
            now() + Duration::hours(5),
        )
        .expect("hold");
    scheduling
        .reschedule(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "they asked to move it",
            now() + Duration::days(1),
        )
        .expect("reschedule");

    // They counter into the emptied round with their own time: it stands.
    let theirs = slot(7, 10);
    let countered_state = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &countered("msg-2", vec![theirs]),
        )
        .expect("their counter lands in the emptied round");
    assert_eq!(countered_state.state(), NegotiationState::Countered);
    assert_eq!(countered_state.standing_slots(), &[theirs][..]);
    assert_eq!(countered_state.offered, Vec::new());

    let error = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(9, 11)],
            Some("act-offer-3".to_string()),
            now() + Duration::days(2),
        )
        .expect_err(
            "new times over their standing counter answer it, and must be recorded as such",
        );
    assert!(
        error.to_string().contains("re_offer"),
        "the refusal names the API that records the answer, got: {error}"
    );

    let unchanged = scheduling
        .load(&scope, &eng(), &opened.negotiation_id)
        .expect("load")
        .expect("present");
    assert_eq!(
        unchanged, countered_state,
        "the refused open recorded nothing"
    );
    assert_eq!(
        unchanged.standing_slots(),
        &[theirs][..],
        "their counter still stands"
    );
    let raw = std::fs::read_to_string(only_log_file(tmp.path())).expect("raw log");
    assert_eq!(
        raw.matches("\"record\":\"offered\"").count(),
        0,
        "no `Offered` line landed over the answered round"
    );
}

/// The failure this pins: the reply dedupe read only the current round's
/// `replies`, and a re-offer clears them — so an inbox item absorbed before
/// the re-offer double-recorded when the re-read inbox re-delivered it after:
/// the dead round's decline resurrected as a fresh reply against the NEW
/// offer. The seen-source memory now survives the round reset, and the replay
/// is a no-op.
#[test]
fn a_source_absorbed_before_a_re_offer_cannot_double_record_after_it() {
    let (tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &declined("msg-1", Some("neither works")),
        )
        .expect("decline");
    let re_offered = scheduling
        .re_offer(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &[slot(3, 10)],
            Some("act-offer-2".to_string()),
            now() + Duration::days(1),
        )
        .expect("re-offer");
    assert_eq!(
        re_offered.replies,
        Vec::new(),
        "the round reset around the new offer"
    );

    let replay = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &declined("msg-1", Some("neither works")),
        )
        .expect("a re-delivered source is not a new reply");
    assert_eq!(replay, re_offered, "the replay recorded nothing");
    assert_eq!(
        replay.replies,
        Vec::new(),
        "the dead round's decline did not resurrect against the new offer"
    );
    assert_eq!(replay.state(), NegotiationState::AwaitingReply);

    let raw = std::fs::read_to_string(only_log_file(tmp.path())).expect("raw log");
    assert_eq!(
        raw.matches("\"record\":\"replied\"").count(),
        1,
        "the log holds exactly the one absorption"
    );
}

/// The failure this pins, across generations: reopening after a close
/// replaced the whole fold entry with the fresh round, erasing the
/// seen-source memory — so an inbox item absorbed in a past round
/// double-recorded into the reopened one when the re-read inbox re-delivered
/// it. The memory carries across the supersede: the replayed source is a
/// no-op in the new round, while a genuinely new message still records.
#[test]
fn a_source_absorbed_in_a_closed_round_cannot_double_record_after_a_reopen() {
    let (tmp, scheduling, scope) = fixture();
    let q3 = open_ask(&scheduling, &scope);
    scheduling
        .absorb(
            &scope,
            &eng(),
            &q3.negotiation_id,
            &declined("msg-1", Some("neither works")),
        )
        .expect("decline");
    scheduling
        .close(
            &scope,
            &eng(),
            &q3.negotiation_id,
            "they went quiet",
            now() + Duration::days(3),
        )
        .expect("close");

    let q4 = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(90, 9)],
            Some("act-offer-q4".to_string()),
            now() + Duration::days(90),
        )
        .expect("reopen");
    assert_eq!(q4.replies, Vec::new());

    let replay = scheduling
        .absorb(
            &scope,
            &eng(),
            &q4.negotiation_id,
            &declined("msg-1", Some("neither works")),
        )
        .expect("a re-delivered source from a past round is not a new reply");
    assert_eq!(
        replay, q4,
        "the replay recorded nothing into the reopened round"
    );
    assert_eq!(replay.replies, Vec::new());

    // A genuinely new message still records into the new round.
    let fresh = scheduling
        .absorb(&scope, &eng(), &q4.negotiation_id, &declined("msg-2", None))
        .expect("their new reply");
    assert_eq!(fresh.replies.len(), 1);
    assert_eq!(fresh.replies[0].source_ref, "msg-2");
    assert_eq!(fresh.state(), NegotiationState::Declined);

    let raw = std::fs::read_to_string(only_log_file(tmp.path())).expect("raw log");
    assert_eq!(
        raw.matches("\"record\":\"replied\"").count(),
        2,
        "one absorption per distinct source, ever"
    );
}

/// The failure this pins: the closed-round supersede arm in `open` matched
/// BEFORE the identical-replay resume, so a retried open — the duplicate
/// delivery of the very request that opened the round — landing after the
/// owner's close appended a fresh `Opened` and silently REOPENED the closed
/// round: the ask read as awaiting a reply the owner had already ended. An
/// identical replay (same slots, same offer act ref as the closed round) now
/// returns the closed record unchanged; only a genuinely new open starts the
/// next round.
#[test]
fn a_retried_open_landing_after_the_close_resumes_the_closed_round() {
    let (tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let closed = scheduling
        .close(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "they went with someone else",
            now() + Duration::hours(2),
        )
        .expect("close");

    let replay = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(1, 9), slot(2, 14)],
            Some("act-offer-1".to_string()),
            now() + Duration::hours(3),
        )
        .expect("the late-landing retry of the original open");
    assert_eq!(
        replay, closed,
        "the replay resumes the closed record; it does not reopen the ask"
    );
    assert_eq!(replay.state(), NegotiationState::Closed);
    assert_eq!(
        replay.closed.as_ref().map(|closed| closed.reason.as_str()),
        Some("they went with someone else")
    );
    assert_eq!(
        replay.closed.as_ref().map(|closed| closed.at),
        Some(now() + Duration::hours(2))
    );
    let raw = std::fs::read_to_string(only_log_file(tmp.path())).expect("raw log");
    assert_eq!(
        raw.matches("\"record\":\"opened\"").count(),
        1,
        "the replay appended nothing"
    );

    // Only a genuinely NEW open starts the next round.
    let next = scheduling
        .open(
            &scope,
            &eng(),
            "dana",
            "quarterly review",
            &[slot(30, 9)],
            Some("act-offer-2".to_string()),
            now() + Duration::days(30),
        )
        .expect("the next round");
    assert_eq!(next.closed, None);
    assert_eq!(next.offered, vec![slot(30, 9)]);
    assert_eq!(next.state(), NegotiationState::AwaitingReply);
    let raw = std::fs::read_to_string(only_log_file(tmp.path())).expect("raw log");
    assert_eq!(
        raw.matches("\"record\":\"opened\"").count(),
        2,
        "the new round's open landed"
    );
}

/// The failure this pins: `silence_follow_up` goes to `None` the moment any
/// reply arrives, so an obligation Module D had already recorded for RIPENED
/// silence became unsettleable the instant the counterparty finally answered
/// — the tuple that identifies the chase in the obligation store was
/// unrecoverable. `silence_settlement` re-derives the SAME tuple from the
/// round's original `offered_at`, field for field, so the answered chase can
/// be settled — and a later close neither revokes it nor moves it.
#[test]
fn an_answer_after_ripened_silence_yields_the_settlement_for_the_recorded_chase() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let window = Duration::hours(48);
    let ripens = now() + window;

    // The chase a sweep recorded while the silence stood.
    let chase = opened
        .silence_follow_up(window, ripens + Duration::hours(1))
        .expect("the silence ripened");

    // They answer a full day after the silence ripened.
    let late_decline = Reply {
        source_ref: "msg-late".to_string(),
        at: now() + Duration::hours(72),
        kind: ReplyKind::Declined {
            reason: Some("was travelling".to_string()),
        },
    };
    let answered = scheduling
        .absorb(&scope, &eng(), &opened.negotiation_id, &late_decline)
        .expect("their late reply");
    assert!(
        answered
            .silence_follow_up(window, ripens + Duration::days(30))
            .is_none(),
        "the follow-up path ends the moment they answer — exactly why the settlement must exist"
    );

    let settlement = answered
        .silence_settlement(window)
        .expect("a chase recorded for ripened silence must stay settleable after the answer");
    assert_eq!(
        settlement.due_at, ripens,
        "the round's ripening instant, never the reply's time"
    );
    assert_eq!(settlement.due_at, chase.due_at);
    assert_eq!(
        settlement.what,
        "a reply to the scheduling ask to dana about quarterly review"
    );
    assert_eq!(settlement.what, chase.what);
    assert_eq!(settlement.audience, chase.audience);
    assert_eq!(settlement.direction, ObligationDirection::OwedToUs);
    assert_eq!(settlement.direction, chase.direction);
    assert_eq!(settlement.created_by, "scheduling");
    assert_eq!(settlement.created_by, chase.created_by);
    assert_eq!(settlement.source_act_ref, Some("act-offer-1".to_string()));
    assert_eq!(settlement.source_act_ref, chase.source_act_ref);
    assert_eq!(settlement.program_id, None);
    assert_eq!(settlement.program_id, chase.program_id);

    // The FIRST answer fixes the verdict: a close arriving later neither
    // revokes the settlement nor moves its tuple.
    let closed = scheduling
        .close(
            &scope,
            &eng(),
            &opened.negotiation_id,
            "answered too late; moved on",
            now() + Duration::days(5),
        )
        .expect("close");
    let after_close = closed
        .silence_settlement(window)
        .expect("still settleable after the close");
    assert_eq!(after_close.due_at, ripens);
    assert_eq!(after_close.what, chase.what);
}

/// The settlement pair's other half: when the answer arrived BEFORE the
/// silence ripened, no chase was ever derivable — a sweep inside the window
/// saw no silence, and a sweep after it sees the answer — so there is nothing
/// to settle, and inventing one would settle an obligation that was never
/// recorded. Both fns return `None`, keyed off the answer's own recorded
/// time, never off when it was absorbed or swept.
#[test]
fn an_answer_before_the_silence_ripened_yields_neither_follow_up_nor_settlement() {
    let (_tmp, scheduling, scope) = fixture();
    let opened = open_ask(&scheduling, &scope);
    let window = Duration::hours(48);

    // The helper's reply time is four hours after the offer — well inside
    // the window — even though the sweep asking is a month later.
    let answered = scheduling
        .absorb(
            &scope,
            &eng(),
            &opened.negotiation_id,
            &declined("msg-1", None),
        )
        .expect("their prompt decline");
    assert_eq!(answered.replies[0].at, now() + Duration::hours(4));
    assert!(answered
        .silence_follow_up(window, now() + Duration::days(30))
        .is_none());
    assert!(
        answered.silence_settlement(window).is_none(),
        "the silence never ripened before their reply, so no chase exists to settle"
    );

    // Same verdict when the round's first answer is a close, not a reply.
    let other = scheduling
        .open(
            &scope,
            &eng(),
            "mira",
            "kickoff",
            &[slot(1, 9)],
            None,
            now(),
        )
        .expect("open");
    let closed = scheduling
        .close(
            &scope,
            &eng(),
            &other.negotiation_id,
            "overtaken by events",
            now() + Duration::hours(1),
        )
        .expect("closed inside the window");
    assert!(
        closed.silence_settlement(window).is_none(),
        "a close before ripening also means no chase was ever derivable"
    );
}

/// Every negotiation in the scope comes back, across relationships, and an
/// unreadable log is not "nothing to sweep".
///
/// Pins the blindness that kept the silence sweep unreachable: logs are named
/// for a hash of the audience key, so a sweep asking *"which asks has nobody
/// answered"* could only ever see relationships somebody had already named —
/// and the asks it must not miss are exactly the ones nobody remembered.
#[test]
fn every_negotiation_in_the_scope_is_listed_across_relationships() {
    let (tmp, scheduling, scope) = fixture();
    let one = open_ask(&scheduling, &scope);
    let two = scheduling
        .open(
            &scope,
            &AudienceRef::panel("panel-9"),
            "the review panel",
            "readout",
            &[slot(3, 10)],
            None,
            now(),
        )
        .expect("open on a different relationship");

    let listed = scheduling.all_negotiations(&scope).expect("list");
    assert_eq!(listed.len(), 2);
    let ids: Vec<&str> = listed
        .iter()
        .map(|held| held.negotiation_id.as_str())
        .collect();
    assert!(ids.contains(&one.negotiation_id.as_str()));
    assert!(ids.contains(&two.negotiation_id.as_str()));
    // Audience key order: `engagement:eng-1` before `panel:panel-9`.
    assert_eq!(listed[0].audience.as_key(), "engagement:eng-1");
    assert_eq!(listed[1].audience.as_key(), "panel:panel-9");
    assert_eq!(listed[1].counterparty, "the review panel");

    assert!(
        scheduling
            .all_negotiations(&SchedulingScope::new("someone-else", "default"))
            .expect("list an untouched scope")
            .is_empty(),
        "a scope that has never opened an ask has none"
    );

    let dir = ArtifactV2Workspace::new(tmp.path())
        .scope_root(&scope.principal, &scope.workspace)
        .join("scheduling");
    let log = std::fs::read_dir(&dir)
        .expect("scheduling dir")
        .map(|entry| entry.expect("dir entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("one log");
    let mut raw = std::fs::read_to_string(&log).expect("read");
    raw.push_str("{not json}\n");
    std::fs::write(&log, raw).expect("write");
    let error = scheduling
        .all_negotiations(&scope)
        .expect_err("an unreadable log must refuse, never fold to an empty listing");
    assert!(
        format!("{error:#}").contains("unparseable record"),
        "{error:#}"
    );
}
