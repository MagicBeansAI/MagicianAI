//! The reconciliation contract, as behaviour.
//!
//! Every test names the exact failure it pins, because each of these is a bug
//! that either happened or was one review away from happening.

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::*;

const ACCEPTED: DeliveryState = DeliveryState::Accepted;
const SOFT: DeliveryState = DeliveryState::Bounced { hard: false };
const FAILED: DeliveryState = DeliveryState::Failed;
const DELIVERED: DeliveryState = DeliveryState::Delivered;
const HARD: DeliveryState = DeliveryState::Bounced { hard: true };
const COMPLAINED: DeliveryState = DeliveryState::Complained;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn ledger() -> (tempfile::TempDir, DeliveryLedger, DeliveryScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let ledger = DeliveryLedger::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, ledger, DeliveryScope::new("anonymous", "default"))
}

fn receipt(
    message_id: &str,
    identity: &str,
    state: DeliveryState,
    observed_at: DateTime<Utc>,
) -> DeliveryReceipt {
    DeliveryReceipt {
        provider: "agentmail".to_string(),
        provider_message_id: message_id.to_string(),
        identity: identity.to_string(),
        state,
        observed_at,
        payload_ref: format!("payload://{message_id}/{}", state.as_str()),
    }
}

/// The order itself, every edge written out.
///
/// Pins: nothing in the lattice drifts silently. A refactor that reordered
/// `severity` — or that let a state follow itself, or let a delivery follow a
/// complaint — would move a real decision about whether somebody may be
/// contacted again, and the only place that decision is visible is this table.
///
/// Columns are `DeliveryState::ALL`, weakest first.
#[test]
fn the_partial_order_admits_exactly_these_edges() {
    let table: [(DeliveryState, [bool; 6]); 6] = [
        //                     accepted  soft   failed  delivered  hard   complained
        (ACCEPTED, [false, true, true, true, true, true]),
        (SOFT, [false, false, true, true, true, true]),
        (FAILED, [false, false, false, true, true, true]),
        (DELIVERED, [false, false, false, false, true, true]),
        (HARD, [false, false, false, false, false, true]),
        (COMPLAINED, [false, false, false, false, false, false]),
    ];

    for (current, admitted) in table {
        for (column, next) in DeliveryState::ALL.into_iter().enumerate() {
            assert_eq!(
                may_follow(current, next),
                admitted[column],
                "`{}` following `{}`",
                next.as_str(),
                current.as_str()
            );
            // The fold's operator must agree with the same table: whichever of
            // the pair may follow the other is the one that survives.
            let expected = if admitted[column] { next } else { current };
            assert_eq!(dominant(current, next), expected);
            assert_eq!(
                dominant(next, current),
                expected,
                "the fold must not depend on which webhook arrived first"
            );
        }
    }
}

/// Pins: a provider that redelivers its webhooks out of order cannot change the
/// answer. Before the fold became a maximum, `delivered → complained` and
/// `complained → delivered` settled differently — a bug that only reproduces
/// under load, which is where it would have been found.
#[test]
fn arrival_order_cannot_decide_the_truth() {
    let (_forward, forward, scope) = ledger();
    forward
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect("delivered");
    forward
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", COMPLAINED, now() + Duration::hours(1)),
            now() + Duration::hours(1),
        )
        .expect("complained");

    let (_reverse, reverse, _) = ledger();
    reverse
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", COMPLAINED, now() + Duration::hours(1)),
            now() + Duration::hours(1),
        )
        .expect("complained");
    reverse
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect("delivered");

    assert_eq!(
        forward.state_of(&scope, "act-1").expect("forward"),
        DeliveryKnowledge::Observed(COMPLAINED)
    );
    assert_eq!(
        reverse.state_of(&scope, "act-1").expect("reverse"),
        DeliveryKnowledge::Observed(COMPLAINED)
    );
}

/// Pins: a late `delivered` does not un-complain an act.
///
/// The complaint is the strongest consent signal a provider hands us. Letting a
/// later delivery event overwrite it would drop somebody's objection on the
/// floor and leave them in the sendable pool.
#[test]
fn a_delivery_arriving_after_a_complaint_changes_nothing() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", COMPLAINED, now()),
            now(),
        )
        .expect("complaint");

    let late = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now() + Duration::hours(2)),
            now() + Duration::hours(2),
        )
        .expect("a refused observation is still recorded");

    assert_eq!(
        late.disposition,
        Reconciliation::Superseded { held: COMPLAINED }
    );
    assert_eq!(late.identity_state, COMPLAINED);
    assert_eq!(late.act_state, COMPLAINED);
    assert_eq!(
        ledger.state_of(&scope, "act-1").expect("state"),
        DeliveryKnowledge::Observed(COMPLAINED)
    );
    // Refused is not discarded: the audit trail holds both.
    let states: Vec<DeliveryState> = ledger
        .observations(&scope, "act-1")
        .expect("observations")
        .into_iter()
        .map(|row| row.state)
        .collect();
    assert_eq!(states, vec![COMPLAINED, DELIVERED]);
}

/// Pins: a hard bounce is terminal, so a stale `delivered` cannot return a dead
/// address to the sendable pool.
#[test]
fn a_delivery_arriving_after_a_hard_bounce_does_not_resurrect_the_address() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", HARD, now()),
            now(),
        )
        .expect("hard bounce");

    let late = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now() + Duration::hours(3)),
            now() + Duration::hours(3),
        )
        .expect("recorded");

    assert_eq!(late.disposition, Reconciliation::Superseded { held: HARD });
    assert_eq!(
        ledger.state_of(&scope, "act-1").expect("state"),
        DeliveryKnowledge::Observed(HARD)
    );
    assert!(!ledger.state_of(&scope, "act-1").expect("state").reached());
}

/// Pins: a *soft* bounce is not terminal. A mailbox that was full on Tuesday is
/// not a dead address, and refusing the retry's delivery would report a
/// successful send as a failure forever.
#[test]
fn a_soft_bounce_still_ripens_into_a_delivery() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", SOFT, now()),
            now(),
        )
        .expect("soft bounce");

    let retry = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now() + Duration::hours(1)),
            now() + Duration::hours(1),
        )
        .expect("delivered");

    assert_eq!(retry.disposition, Reconciliation::Advanced { from: SOFT });
    assert_eq!(retry.identity_state, DELIVERED);
    assert_eq!(
        ledger.state_of(&scope, "act-1").expect("state"),
        DeliveryKnowledge::Observed(DELIVERED)
    );
}

/// Pins: a redelivered webhook is one record, not two.
///
/// Providers redeliver aggressively. Without idempotency by provider message id
/// a single bounce would be counted five times, and every count this ledger
/// reports would be fiction.
#[test]
fn an_identical_receipt_twice_is_one_record() {
    let (_tmp, ledger, scope) = ledger();
    let body = receipt("m-1", "ada@x.test", DELIVERED, now());
    let first = ledger
        .reconcile(&scope, "act-1", &body, now())
        .expect("first");
    let second = ledger
        .reconcile(&scope, "act-1", &body, now() + Duration::minutes(5))
        .expect("redelivery");

    assert_eq!(second.disposition, Reconciliation::Replayed);
    assert_eq!(second.observation, first.observation);
    assert_eq!(
        ledger.observations(&scope, "act-1").expect("observations"),
        vec![first.observation]
    );
}

/// Pins: two different stories under one provider message id are an error, not
/// a silently-ignored second one.
///
/// If a provider reuses ids — or an adapter mis-parses them — quietly keeping
/// the first receipt hides the collision until the day the two disagree about
/// who was contacted.
#[test]
fn a_changed_receipt_under_one_provider_id_is_an_error_not_a_no_op() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect("first");

    let mut changed = receipt("m-1", "bob@x.test", DELIVERED, now());
    changed.payload_ref = "payload://tampered".to_string();
    let error = ledger
        .reconcile(&scope, "act-1", &changed, now())
        .expect_err("a changed receipt must not resume");
    assert!(
        error.to_string().contains("two different stories"),
        "{error}"
    );

    // The refusal left the original untouched.
    assert_eq!(
        ledger
            .state_for(&scope, "act-1", "ada@x.test")
            .expect("original"),
        DeliveryKnowledge::Observed(DELIVERED)
    );
}

/// Pins: a *different state* under the same provider message id is the message
/// moving on, not a duplicate.
///
/// Had the receipt id been derived without the state, the bounce would have
/// resumed the acceptance's row and the transition would have been swallowed —
/// the act would still read `accepted` while the address was dead.
#[test]
fn a_different_state_under_one_provider_id_is_a_transition() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", ACCEPTED, now()),
            now(),
        )
        .expect("accepted");
    let bounced = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", HARD, now() + Duration::minutes(2)),
            now() + Duration::minutes(2),
        )
        .expect("bounced");

    assert_eq!(
        bounced.disposition,
        Reconciliation::Advanced { from: ACCEPTED }
    );
    let states: Vec<DeliveryState> = ledger
        .observations(&scope, "act-1")
        .expect("observations")
        .into_iter()
        .map(|row| row.state)
        .collect();
    assert_eq!(states, vec![ACCEPTED, HARD]);
}

/// Pins: the whole reason this module exists. An act nothing has reconciled
/// reads as `dispatch_unknown`, and `dispatch_unknown` is never delivery.
#[test]
fn dispatch_unknown_never_reads_as_delivered() {
    let (_tmp, ledger, scope) = ledger();
    let unknown = ledger
        .state_of(&scope, "act-never-heard-of")
        .expect("state");

    assert_eq!(unknown, DeliveryKnowledge::DispatchUnknown);
    assert_eq!(unknown.as_str(), "dispatch_unknown");
    assert!(!unknown.reached());
    assert!(!unknown.is_reconciled());
    assert_eq!(unknown.state(), None);
    assert_ne!(unknown, DeliveryKnowledge::Observed(DELIVERED));
}

/// Pins: provider *acceptance* is not delivery. Reading it as success is the
/// same mistake the three-way split in the outward act's status already refused
/// once, and this ledger must not reintroduce it.
#[test]
fn an_acceptance_is_not_an_arrival() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", ACCEPTED, now()),
            now(),
        )
        .expect("accepted");

    let state = ledger.state_of(&scope, "act-1").expect("state");
    assert_eq!(state, DeliveryKnowledge::Observed(ACCEPTED));
    assert!(state.is_reconciled(), "the provider did say something");
    assert!(!state.reached(), "but it never said it arrived");
    assert!(!ACCEPTED.is_terminal());
}

/// Pins: fail closed. An unreadable ledger must not answer "nothing bounced" —
/// that manufactures confidence for exactly the send that should be held.
#[test]
fn an_unreadable_ledger_is_an_error_never_an_empty_one() {
    let (_tmp, ledger, scope) = ledger();
    // A directory where the log belongs: readable-file absence is NotFound, and
    // this is every other fault.
    std::fs::create_dir_all(ledger.act_path(&scope, "act-1")).expect("obstruction");

    let error = ledger
        .state_of(&scope, "act-1")
        .expect_err("an I/O fault must surface");
    assert!(error.to_string().contains("unreadable log"), "{error}");
}

/// Pins: silence is derived from the clock, and the boundary is inclusive.
///
/// Nothing writes a `stale` flag, so an act is overdue whether or not any sweep
/// has ever run — and an act silent for exactly the grace period is overdue,
/// like every other expiry in this codebase.
#[test]
fn unreconciled_derives_silence_from_the_clock_and_includes_the_boundary() {
    let (_tmp, ledger, scope) = ledger();
    let grace = Duration::hours(6);
    let dispatched = vec![
        DispatchedAct::new("act-exactly-due", now() - grace),
        DispatchedAct::new("act-too-fresh", now() - Duration::hours(3)),
    ];

    let found = ledger
        .unreconciled(&scope, &dispatched, grace, now())
        .expect("sweep");
    assert_eq!(
        found,
        vec![UnreconciledAct {
            act_ref: "act-exactly-due".to_string(),
            dispatched_at: now() - grace,
            silent_for: grace,
        }]
    );
}

/// Pins: a sweep over nothing is not a clean bill of health.
///
/// An empty `Vec` from a health check reads as "all good"; over an empty
/// candidate list it means "nothing was checked". Vacuous truth dressed as
/// reassurance is refused outright.
#[test]
fn a_sweep_over_no_candidates_is_refused() {
    let (_tmp, ledger, scope) = ledger();
    let error = ledger
        .unreconciled(&scope, &[], Duration::hours(1), now())
        .expect_err("an empty sweep must not pass");
    assert!(
        error.to_string().contains("clean bill of health"),
        "{error}"
    );

    let tally = ledger
        .tally(&scope, &[])
        .expect_err("an all-zero tally must not pass either");
    assert!(tally.to_string().contains("clean week"), "{tally}");

    let reach = ledger
        .reach(&scope, "act-1", &[])
        .expect_err("a reach report over nobody must not pass");
    assert!(reach.to_string().contains("vacuous pass"), "{reach}");
}

/// Pins: a doubled candidate is one finding, and a repeat cannot shorten the
/// recorded silence. A caller assembling candidates from two sources would
/// otherwise see the same broken act twice and read it as two incidents.
#[test]
fn a_repeated_candidate_is_one_finding_at_its_earliest_dispatch() {
    let (_tmp, ledger, scope) = ledger();
    let dispatched = vec![
        DispatchedAct::new("act-1", now() - Duration::hours(9)),
        DispatchedAct::new("act-1", now() - Duration::hours(2)),
    ];

    let found = ledger
        .unreconciled(&scope, &dispatched, Duration::hours(1), now())
        .expect("sweep");
    assert_eq!(
        found,
        vec![UnreconciledAct {
            act_ref: "act-1".to_string(),
            dispatched_at: now() - Duration::hours(9),
            silent_for: Duration::hours(9),
        }]
    );
}

/// Pins: acknowledged means the provider said *something*. An act sitting at a
/// bare acceptance is reconciled — its problem is a different one, and reporting
/// it as unacknowledged would bury the acts nobody ever heard about.
#[test]
fn an_acknowledged_act_is_not_unreconciled() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-heard",
            &receipt("m-1", "ada@x.test", ACCEPTED, now() - Duration::days(2)),
            now() - Duration::days(2),
        )
        .expect("accepted");

    let dispatched = vec![
        DispatchedAct::new("act-heard", now() - Duration::days(2)),
        DispatchedAct::new("act-silent", now() - Duration::days(2)),
    ];
    let found: Vec<String> = ledger
        .unreconciled(&scope, &dispatched, Duration::hours(1), now())
        .expect("sweep")
        .into_iter()
        .map(|act| act.act_ref)
        .collect();
    assert_eq!(found, vec!["act-silent".to_string()]);
}

/// Pins: only hard bounces and complaints are suppression signals.
///
/// A soft bounce is transient, and suppressing on transient failures is how a
/// sender quietly deletes its own audience. A delivery is not a signal at all.
#[test]
fn suppression_signals_carry_hard_bounces_and_complaints_and_nothing_else() {
    let (_tmp, ledger, scope) = ledger();
    for (act, message, identity, state) in [
        ("act-1", "m-1", "ada@x.test", HARD),
        ("act-2", "m-2", "bob@x.test", COMPLAINED),
        ("act-3", "m-3", "cyd@x.test", SOFT),
        ("act-4", "m-4", "dee@x.test", DELIVERED),
        ("act-5", "m-5", "eli@x.test", FAILED),
    ] {
        ledger
            .reconcile(
                &scope,
                act,
                &receipt(message, identity, state, now()),
                now(),
            )
            .expect("reconciled");
    }

    let signals = ledger
        .suppression_signals(&scope, now() - Duration::days(1))
        .expect("signals");
    assert_eq!(
        signals,
        vec![
            ("ada@x.test".to_string(), SuppressionCause::HardBounce),
            ("bob@x.test".to_string(), SuppressionCause::Complaint),
        ]
    );
    assert_eq!(SOFT.suppression_cause(), None);
    assert_eq!(DELIVERED.suppression_cause(), None);
    assert_eq!(FAILED.suppression_cause(), None);
}

/// Pins: the sweep window is on OUR clock, inclusively.
///
/// Providers backdate. A complaint that arrives today carrying last week's
/// timestamp would fall behind a sweep that had already passed last week, and
/// the objection would be lost forever — filtering on when we *learned* it is
/// what keeps a late signal catchable by the next sweep.
#[test]
fn a_backdated_signal_is_still_caught_by_the_next_sweep() {
    let (_tmp, ledger, scope) = ledger();
    let last_sweep = now() - Duration::hours(1);

    // Old news, learned before the last sweep: already ingested, not re-offered.
    ledger
        .reconcile(
            &scope,
            "act-old",
            &receipt("m-old", "old@x.test", HARD, now() - Duration::days(9)),
            now() - Duration::days(9),
        )
        .expect("old");
    // Backdated by the provider, learned now.
    ledger
        .reconcile(
            &scope,
            "act-late",
            &receipt(
                "m-late",
                "late@x.test",
                COMPLAINED,
                now() - Duration::days(9),
            ),
            now(),
        )
        .expect("late");
    // Learned exactly on the boundary: inclusive, so it is offered.
    ledger
        .reconcile(
            &scope,
            "act-edge",
            &receipt("m-edge", "edge@x.test", HARD, now() - Duration::days(9)),
            last_sweep,
        )
        .expect("edge");

    let signals = ledger
        .suppression_signals(&scope, last_sweep)
        .expect("signals");
    assert_eq!(
        signals,
        vec![
            ("edge@x.test".to_string(), SuppressionCause::HardBounce),
            ("late@x.test".to_string(), SuppressionCause::Complaint),
        ]
    );
}

/// Pins: one provider message belongs to one act.
///
/// Two acts claiming the same provider message would let a bounce or a complaint
/// land on a disclosure that never carried it — suppressing the wrong person, or
/// correcting the wrong email.
#[test]
fn one_provider_message_cannot_belong_to_two_acts() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", ACCEPTED, now()),
            now(),
        )
        .expect("first act");

    let error = ledger
        .reconcile(
            &scope,
            "act-2",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect_err("a second act must not claim it");
    assert!(
        error
            .to_string()
            .contains("one provider message is one act"),
        "{error}"
    );
    assert_eq!(
        ledger.state_of(&scope, "act-2").expect("untouched"),
        DeliveryKnowledge::DispatchUnknown
    );
}

/// Pins: every caller string that feeds an id derivation refuses U+001F.
///
/// It is the separator holding a receipt id's components apart. A crafted value
/// could otherwise shift a component boundary and resume — or shadow — another
/// act's record.
#[test]
fn a_unit_separator_in_any_caller_string_is_refused() {
    let (_tmp, ledger, scope) = ledger();
    let sep = '\u{1f}';

    let cases: Vec<(&str, anyhow::Error)> = vec![
        (
            "act ref",
            ledger
                .reconcile(
                    &scope,
                    &format!("act{sep}1"),
                    &receipt("m-1", "ada@x.test", DELIVERED, now()),
                    now(),
                )
                .expect_err("act ref"),
        ),
        (
            "provider message id",
            ledger
                .reconcile(
                    &scope,
                    "act-1",
                    &receipt(&format!("m{sep}1"), "ada@x.test", DELIVERED, now()),
                    now(),
                )
                .expect_err("message id"),
        ),
        (
            "identity",
            ledger
                .reconcile(
                    &scope,
                    "act-1",
                    &receipt("m-1", &format!("ada{sep}@x.test"), DELIVERED, now()),
                    now(),
                )
                .expect_err("identity"),
        ),
    ];
    for (what, error) in cases {
        assert!(error.to_string().contains("U+001F"), "{what}: {error}");
    }

    let crafted_scope = DeliveryScope::new(format!("anon{sep}ymous"), "default");
    let error = ledger
        .reconcile(
            &crafted_scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect_err("scope");
    assert!(error.to_string().contains("U+001F"), "{error}");
}

/// Pins: an act ref arrives from outside and never becomes a path component.
///
/// A caller-supplied string used raw in a file name is a directory traversal
/// waiting for its first hostile webhook.
#[test]
fn an_act_ref_cannot_walk_out_of_its_scope_directory() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "../../escape",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect("hostile act ref is stored, not obeyed");

    let names: Vec<String> = std::fs::read_dir(ledger.root(&scope).join("acts"))
        .expect("acts dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert_eq!(names.len(), 1, "one log, inside the scope: {names:?}");
    let stem = names[0].strip_suffix(".jsonl").expect("jsonl log");
    assert_eq!(stem.len(), 32);
    assert!(
        stem.chars().all(|character| character.is_ascii_hexdigit()),
        "{stem}"
    );
    assert_eq!(
        ledger
            .state_of(&scope, "../../escape")
            .expect("still readable"),
        DeliveryKnowledge::Observed(DELIVERED)
    );
}

/// Pins: vacuous truth. "Every one of the zero recipients received it" is how a
/// broken audience resolution reports a clean send.
#[test]
fn everyone_reached_is_false_over_an_audience_of_nobody() {
    let empty = ReachReport {
        act_ref: "act-1".to_string(),
        checked: 0,
        reached: Vec::new(),
        fell_short: Vec::new(),
        unobserved: Vec::new(),
    };
    assert!(!empty.everyone_reached());

    let real = ReachReport {
        act_ref: "act-1".to_string(),
        checked: 1,
        reached: vec!["ada@x.test".to_string()],
        fell_short: Vec::new(),
        unobserved: Vec::new(),
    };
    assert!(real.everyone_reached());
}

/// Pins: silence and failure are different answers.
///
/// A recipient the provider never mentioned is `dispatch_unknown`, not a
/// failure and certainly not a success. Folding the two together would hide a
/// half-wired integration behind a bounce count that looked plausible.
#[test]
fn reach_separates_silence_from_failure() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect("delivered");
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-2", "bob@x.test", HARD, now()),
            now(),
        )
        .expect("bounced");

    let report = ledger
        .reach(
            &scope,
            "act-1",
            &[
                "ada@x.test".to_string(),
                "bob@x.test".to_string(),
                "cyd@x.test".to_string(),
            ],
        )
        .expect("reach");

    assert_eq!(
        report,
        ReachReport {
            act_ref: "act-1".to_string(),
            checked: 3,
            reached: vec!["ada@x.test".to_string()],
            fell_short: vec![("bob@x.test".to_string(), HARD)],
            unobserved: vec!["cyd@x.test".to_string()],
        }
    );
    assert!(!report.everyone_reached());
}

/// Pins: one act holds one story per recipient.
///
/// The order governs a single message's life. Reading a tenth person's bounce as
/// this person's is how a working address gets suppressed — while the act-level
/// fold still surfaces that something went wrong for somebody.
#[test]
fn recipients_are_folded_apart() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect("ada");
    let bob = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-2", "bob@x.test", HARD, now()),
            now(),
        )
        .expect("bob");

    // Bob's bounce opens BOB's story; it is not a transition on Ada's.
    assert_eq!(bob.disposition, Reconciliation::Opened);
    assert_eq!(bob.identity_state, HARD);
    assert_eq!(bob.act_state, HARD, "the act went wrong for somebody");

    assert_eq!(
        ledger
            .state_for(&scope, "act-1", "ada@x.test")
            .expect("ada"),
        DeliveryKnowledge::Observed(DELIVERED)
    );
    assert_eq!(
        ledger
            .state_for(&scope, "act-1", "bob@x.test")
            .expect("bob"),
        DeliveryKnowledge::Observed(HARD)
    );
    assert_eq!(
        ledger
            .state_for(&scope, "act-1", "cyd@x.test")
            .expect("cyd"),
        DeliveryKnowledge::DispatchUnknown
    );
}

/// Pins: counts, and the denominator behind them. A rate would render six
/// bounces over eight acts and over eight thousand identically, and the two are
/// not the same fact.
#[test]
fn the_tally_reports_counts_and_the_denominator_behind_them() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", DELIVERED, now()),
            now(),
        )
        .expect("delivered");
    ledger
        .reconcile(
            &scope,
            "act-2",
            &receipt("m-2", "bob@x.test", HARD, now()),
            now(),
        )
        .expect("hard");
    ledger
        .reconcile(
            &scope,
            "act-3",
            &receipt("m-3", "cyd@x.test", ACCEPTED, now()),
            now(),
        )
        .expect("accepted");

    let tally = ledger
        .tally(
            &scope,
            &[
                "act-1".to_string(),
                "act-2".to_string(),
                "act-3".to_string(),
                "act-4".to_string(),
                // A repeat must not inflate the denominator.
                "act-1".to_string(),
            ],
        )
        .expect("tally");

    assert_eq!(
        tally,
        DeliveryTally {
            acts_checked: 4,
            accepted: 1,
            soft_bounced: 0,
            failed: 0,
            delivered: 1,
            hard_bounced: 1,
            complained: 0,
            dispatch_unknown: 1,
        }
    );
}

/// Pins: an identity is a person, not a string.
///
/// Providers hand back their own capitalisation and their own angle-wrapping of
/// the same mailbox. A ledger that matched literally would report a bounce for
/// `<Ada@X.test>` and a silence for `ada@x.test` — the same person, two answers.
#[test]
fn an_identity_is_matched_as_a_person_not_a_string() {
    let (_tmp, ledger, scope) = ledger();
    let stored = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", " <Ada@X.test> ", HARD, now()),
            now(),
        )
        .expect("bounced");
    assert_eq!(stored.observation.identity, "ada@x.test");

    assert_eq!(
        ledger
            .state_for(&scope, "act-1", "ADA@x.TEST")
            .expect("state"),
        DeliveryKnowledge::Observed(HARD)
    );
    assert_eq!(
        ledger
            .suppression_signals(&scope, now() - Duration::days(1))
            .expect("signals"),
        vec![("ada@x.test".to_string(), SuppressionCause::HardBounce)]
    );
    let blank = normalise_identity("  <>  ").expect_err("a blank identity is uncheckable");
    assert!(
        blank.to_string().contains("an identity is required"),
        "{blank}"
    );
}

/// Pins: a provider id that changes its mind about WHO is refused whatever
/// state it claims — and the refusal writes nothing.
///
/// The identity check used to be keyed on the derived receipt id, which folds
/// the observed state in, so it only fired while the state held still. A second
/// receipt under `m-1` naming a *different* address AND a *different* state
/// derived a different id, missed the guard entirely, and wrote a complaint
/// suppression against `bob@x.test` — who never complained, and whom nothing
/// downstream can un-suppress.
#[test]
fn a_provider_id_that_changes_identity_is_refused_whatever_state_it_claims() {
    let (_tmp, ledger, scope) = ledger();
    ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", ACCEPTED, now()),
            now(),
        )
        .expect("accepted, for ada");

    let error = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "bob@x.test", COMPLAINED, now() + Duration::hours(1)),
            now() + Duration::hours(1),
        )
        .expect_err("one provider message is one identity's story");
    assert!(
        error.to_string().contains("one identity's story"),
        "{error}"
    );

    // Nothing was written: no suppression against bob, no row, no state.
    assert_eq!(
        ledger
            .suppression_signals(&scope, now() - Duration::days(1))
            .expect("signals"),
        Vec::<(String, SuppressionCause)>::new()
    );
    assert_eq!(
        ledger
            .state_for(&scope, "act-1", "bob@x.test")
            .expect("bob"),
        DeliveryKnowledge::DispatchUnknown
    );
    let identities: Vec<String> = ledger
        .observations(&scope, "act-1")
        .expect("observations")
        .into_iter()
        .map(|row| row.identity)
        .collect();
    assert_eq!(identities, vec!["ada@x.test".to_string()]);

    // The legitimate case still works: the SAME identity reporting a new state
    // under the same provider id is a transition, not a duplicate.
    let moved = ledger
        .reconcile(
            &scope,
            "act-1",
            &receipt("m-1", "ada@x.test", COMPLAINED, now() + Duration::hours(2)),
            now() + Duration::hours(2),
        )
        .expect("ada's own transition");
    assert_eq!(
        moved.disposition,
        Reconciliation::Advanced { from: ACCEPTED }
    );
    assert_eq!(moved.identity_state, COMPLAINED);
    assert_eq!(
        ledger
            .suppression_signals(&scope, now() - Duration::days(1))
            .expect("signals"),
        vec![("ada@x.test".to_string(), SuppressionCause::Complaint)]
    );
}

/// One producer holds this ledger, and it is the intake door.
///
/// Pins the header sentence that had drifted into the present tense: *"Email,
/// WhatsApp, SMS and push reconcile through the same ledger"* described four
/// live producers where there were none, standing directly beside the sentence
/// that says every live send stays at `dispatch_unknown` forever. Both could
/// not be true, and a reviewer who trusted the first would have stopped looking
/// for the receipt path that was actually missing.
///
/// That path now exists, and it is exactly one file wide: `delivery::intake`,
/// reached from `POST /api/magician/v2/delivery/receipts`. **When a second
/// module starts recording receipts, this test fails.** Correct the header to
/// name it, and say whether it is a rail or another door — the distinction is
/// the one this whole subsystem turns on.
///
/// The list below is *holders*, which is a weaker fact than *producers*: the
/// delivery-hygiene sweep opens a ledger to READ its suppression signals and
/// records nothing into it. The claim the header still makes about the executor
/// is the second half of this test — that nothing on the send path carries a
/// provider receipt back — and that half is unchanged: the door is walked from
/// outside, by an owner or an operator, never by the adapter that sent.
#[test]
fn only_the_intake_door_reconciles_through_this_ledger() {
    use crate::magician_v2::doc_wiring_scan::scan_workspace;

    const OWN_FILES: [&str; 2] = [
        "magician/src/magician_v2/delivery/mod.rs",
        "magician/src/magician_v2/delivery/tests.rs",
    ];

    let holders = scan_workspace("DeliveryLedger::new", &OWN_FILES);
    assert!(
        holders.files_searched > 100,
        "only {} files were read, so this proves nothing",
        holders.files_searched
    );
    assert_eq!(
        holders.hits,
        vec![
            // The owner-facing read of what was dispatched and never
            // acknowledged. Opens a ledger to ask `unreconciled`; records
            // nothing into it.
            "magician-api/src/suppression_api.rs".to_string(),
            // Builds one solely inside its own tests, to pair `DispatchLog`
            // against `unreconciled`.
            "magician/src/magician_v2/agents/outward_gate.rs".to_string(),
            // THE PRODUCER. `intake::ReceiptIntake::admit` is the only
            // non-test caller of `reconcile` in the workspace: it validates,
            // records who presented the receipt, refuses one whose act this
            // scope never dispatched, and hands the rest to the ledger
            // untouched.
            "magician/src/magician_v2/delivery/intake.rs".to_string(),
            // The hygiene sweep and its worker: both READ `suppression_signals`
            // and neither calls `reconcile`, which is what the assertion below
            // holds them to.
            "magician/src/magician_v2/delivery_hygiene/mod.rs".to_string(),
            // The silence watch: reads `unreconciled` and writes nothing at all.
            "magician/src/magician_v2/delivery_hygiene/silence.rs".to_string(),
            "magician/src/magician_v2/delivery_hygiene/worker.rs".to_string(),
            // Focused receipt fixtures open the real ledger; listing the test
            // module keeps the textual architecture scan fail-closed.
            "magician/src/magician_v2/delivery_receipts/tests.rs".to_string(),
        ],
        "a new module builds a `DeliveryLedger`; the header names the ones that do"
    );

    // The executor writes `dispatch_unknown` on every live send and never a
    // receipt — the exact state the header calls permanent. The tree-wide scan
    // comes first, so a renamed method cannot turn this into a test that passes
    // because its needle stopped existing.
    let anywhere = scan_workspace("record_provider_receipt", &OWN_FILES);
    assert!(
        !anywhere.hits.is_empty(),
        "`record_provider_receipt` matched nothing at all; the needle is stale, so the \
         assertion below would pass vacuously"
    );
    assert_eq!(
        anywhere
            .hits
            .iter()
            .filter(|path| path.contains("/execution/"))
            .count(),
        0,
        "the executor records a provider receipt now; the header says nothing carries one back"
    );
}
