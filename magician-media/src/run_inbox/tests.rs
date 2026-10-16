//! §5's "primitive nobody has", as behaviour.

use chrono::{Duration, TimeZone};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::*;

const INBOX: &str = "agent@example.com";

fn at(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap() + Duration::minutes(minutes)
}

fn fixture() -> (tempfile::TempDir, RunStateStore, RunScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let runs = RunStateStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, runs, RunScope::new("anonymous", "default"))
}

/// A run waiting on one source.
fn waiting_run(runs: &RunStateStore, scope: &RunScope, purpose: &str, source_hint: &str) -> String {
    let run = runs
        .open(
            scope,
            purpose,
            "https://portal.test/apply",
            None,
            &[],
            "owner",
            at(0),
        )
        .expect("run");
    runs.raise_expectation(
        scope,
        &run.run_id,
        "the verification code",
        source_hint,
        at(1),
    )
    .expect("wait");
    run.run_id
}

/// A mailbox holding exactly what it is given.
struct Holding(Vec<InboundEvent>);

impl RunInbox for Holding {
    fn name(&self) -> &str {
        "holding"
    }
    fn messages(&self, _scope: &RunScope, _now: DateTime<Utc>) -> Result<Vec<InboundEvent>> {
        Ok(self.0.clone())
    }
}

/// A mailbox that cannot be read.
struct Broken;

impl RunInbox for Broken {
    fn name(&self) -> &str {
        "broken"
    }
    fn messages(&self, _scope: &RunScope, _now: DateTime<Utc>) -> Result<Vec<InboundEvent>> {
        anyhow::bail!("the mailbox could not be opened")
    }
}

fn mail(event_ref: &str, source_hint: &str, minutes: i64) -> InboundEvent {
    InboundEvent {
        event_ref: event_ref.to_string(),
        source_hint: source_hint.to_string(),
        at: at(minutes),
    }
}

/// The loop that was missing: a message closes the wait it was for, and every
/// other message in the inbox is left alone.
///
/// `unmatched` being the common case is the point — an inbox carries far more
/// than one run's verification, and treating the rest as failures would make
/// the primitive unusable.
#[test]
fn a_message_closes_the_wait_it_was_for_and_the_rest_are_ordinary_mail() {
    let (_tmp, runs, scope) = fixture();
    let run_id = waiting_run(&runs, &scope, "the YC application", INBOX);

    let inbox = Holding(vec![
        mail("msg-newsletter", "someone-else@example.test", 2),
        mail("msg-code", INBOX, 3),
        mail("msg-receipt", "billing@example.test", 4),
    ]);
    let sweep = sweep_run_inbox(&inbox, &runs, &scope, at(5)).expect("sweep");

    assert_eq!(sweep.examined, 3);
    assert_eq!(sweep.accounted_for(), sweep.examined);
    assert_eq!(sweep.runs_waiting, 1);
    assert_eq!(sweep.unmatched, 2, "an inbox is not one run's mailbox");
    assert_eq!(sweep.fulfilled.len(), 1);
    assert_eq!(sweep.fulfilled[0].run_id, run_id);
    assert_eq!(sweep.fulfilled[0].event_ref, "msg-code");
    assert!(sweep.ambiguous.is_empty());

    let run = runs.load(&scope, &run_id).expect("load").expect("run");
    assert!(
        run.expectations.iter().all(|held| !held.is_open()),
        "the wait is closed on the run itself, not only in the report"
    );
}

/// Re-reading the mailbox writes nothing.
///
/// The store's idempotency IS the cursor — which is why this module settles no
/// message and remembers nothing. A cursor would be one more thing to keep
/// honest, and the failure mode of getting it wrong is a verification that
/// arrives and is never seen again.
#[test]
fn a_re_read_mailbox_is_a_no_op_and_says_so() {
    let (_tmp, runs, scope) = fixture();
    waiting_run(&runs, &scope, "the YC application", INBOX);

    let inbox = Holding(vec![mail("msg-code", INBOX, 3)]);
    let first = sweep_run_inbox(&inbox, &runs, &scope, at(5)).expect("first");
    assert_eq!(first.fulfilled.len(), 1);
    assert_eq!(first.already_fulfilled, 0);

    let second = sweep_run_inbox(&inbox, &runs, &scope, at(6)).expect("second");
    assert!(second.fulfilled.is_empty(), "nothing was written twice");
    assert_eq!(
        second.runs_waiting, 0,
        "the run holds no open wait, so it is not waiting any more"
    );
    // Reported as ALREADY DONE, not as ordinary mail. Both are no-ops and they
    // are opposite facts: one says the verification arrived, the other says it
    // never did. Collapsing them would make a re-read inbox indistinguishable
    // from a run still waiting.
    assert_eq!(second.already_fulfilled, 1);
    assert_eq!(second.unmatched, 0);
}

/// Two runs waiting on one source closes NEITHER.
///
/// One message satisfies one wait, and picking is a coin flip whose loser is
/// closed by an event that never satisfied it. `fulfil_from_inbound` already
/// refuses this within a run; across runs the store cannot see it, because it
/// is only ever asked about one run at a time.
#[test]
fn a_source_two_runs_are_waiting_on_closes_neither() {
    let (_tmp, runs, scope) = fixture();
    let first = waiting_run(&runs, &scope, "the YC application", INBOX);
    let second = waiting_run(&runs, &scope, "the Techstars application", INBOX);

    let inbox = Holding(vec![mail("msg-code", INBOX, 3)]);
    let sweep = sweep_run_inbox(&inbox, &runs, &scope, at(5)).expect("sweep");

    assert!(sweep.fulfilled.is_empty(), "a coin flip is not a match");
    assert_eq!(sweep.ambiguous.len(), 1);
    assert_eq!(sweep.ambiguous[0].run_ids.len(), 2);
    assert!(sweep.ambiguous[0].run_ids.contains(&first));
    assert!(sweep.ambiguous[0].run_ids.contains(&second));

    for run_id in [first, second] {
        let run = runs.load(&scope, &run_id).expect("load").expect("run");
        assert!(
            run.expectations.iter().all(|held| held.is_open()),
            "both waits are still open"
        );
    }
}

/// One ambiguous source is reported once, however many messages arrive on it.
///
/// The instruction is "narrow the hints", and repeating it per message buries
/// it under the noise it is trying to fix.
#[test]
fn an_ambiguous_source_is_reported_once_not_once_per_message() {
    let (_tmp, runs, scope) = fixture();
    waiting_run(&runs, &scope, "one", INBOX);
    waiting_run(&runs, &scope, "two", INBOX);

    let inbox = Holding(vec![
        mail("msg-1", INBOX, 3),
        mail("msg-2", INBOX, 4),
        mail("msg-3", INBOX, 5),
    ]);
    let sweep = sweep_run_inbox(&inbox, &runs, &scope, at(6)).expect("sweep");
    assert_eq!(sweep.examined, 3);
    assert_eq!(sweep.ambiguous.len(), 1, "one source, one instruction");
    assert_eq!(
        sweep.ambiguous[0].messages, 3,
        "the source is named once and the messages it held are still counted"
    );
    assert_eq!(
        sweep.accounted_for(),
        sweep.examined,
        "every message has to land in a bucket, or one went somewhere nothing reports"
    );
}

/// Every message lands in exactly one bucket, across every outcome at once.
///
/// The invariant that makes the report readable: a total falling short of
/// `examined` means a message went somewhere nothing reports, which is how a
/// verification that arrived becomes a run that waits forever.
#[test]
fn every_message_is_accounted_for_whatever_happened_to_it() {
    let (_tmp, runs, scope) = fixture();

    // Closed by this pass.
    waiting_run(&runs, &scope, "will close", INBOX);
    // Contested: two runs, one source.
    waiting_run(&runs, &scope, "contested one", "shared@example.test");
    waiting_run(&runs, &scope, "contested two", "shared@example.test");
    // Refused: one run, two open waits on one source.
    let tangled = runs
        .open(
            &scope,
            "tangled",
            "https://portal.test/a",
            None,
            &[],
            "owner",
            at(0),
        )
        .expect("run");
    for description in ["code", "countersignature"] {
        runs.raise_expectation(
            &scope,
            &tangled.run_id,
            description,
            "tangled@example.test",
            at(1),
        )
        .expect("wait");
    }

    let inbox = Holding(vec![
        mail("m-close", INBOX, 2),
        mail("m-contested-a", "shared@example.test", 3),
        mail("m-contested-b", "shared@example.test", 4),
        mail("m-tangled", "tangled@example.test", 5),
        mail("m-newsletter", "nobody@example.test", 6),
    ]);
    let sweep = sweep_run_inbox(&inbox, &runs, &scope, at(9)).expect("sweep");

    assert_eq!(sweep.examined, 5);
    assert_eq!(sweep.fulfilled.len(), 1);
    assert_eq!(sweep.ambiguous.iter().map(|h| h.messages).sum::<usize>(), 2);
    assert_eq!(sweep.refused.len(), 1);
    assert_eq!(sweep.unmatched, 1);
    assert_eq!(sweep.already_fulfilled, 0);
    assert_eq!(sweep.accounted_for(), 5, "one bucket each, none lost");
}

/// The source match ignores case and surrounding whitespace, matching what the
/// store does — or the two would disagree about whether a message matched.
#[test]
fn the_source_match_agrees_with_the_store_about_spelling() {
    let (_tmp, runs, scope) = fixture();
    waiting_run(&runs, &scope, "the YC application", "  Agent@Example.Com ");

    let inbox = Holding(vec![mail("msg-code", "agent@example.com", 3)]);
    let sweep = sweep_run_inbox(&inbox, &runs, &scope, at(5)).expect("sweep");
    assert_eq!(
        sweep.fulfilled.len(),
        1,
        "a reader spelling it differently still matches"
    );
}

/// An unreadable mailbox propagates.
///
/// A run waiting on a verification that already arrived is invisible from every
/// other surface, so a pass reporting success over a mailbox it could not read
/// would hide exactly the state this exists to find.
#[test]
fn an_unreadable_mailbox_is_an_error_not_a_quiet_week() {
    let (_tmp, runs, scope) = fixture();
    waiting_run(&runs, &scope, "the YC application", INBOX);
    assert!(sweep_run_inbox(&Broken, &runs, &scope, at(5)).is_err());
}

/// A scope with no runs looks at the mailbox and says how much it saw.
///
/// `examined: 0` and `examined: 300` are different facts behind the same
/// "closed nothing".
#[test]
fn nothing_waiting_still_reports_the_denominator() {
    let (_tmp, runs, scope) = fixture();
    let inbox = Holding(vec![mail("msg-1", INBOX, 3), mail("msg-2", INBOX, 4)]);
    let sweep = sweep_run_inbox(&inbox, &runs, &scope, at(5)).expect("sweep");
    assert_eq!(sweep.runs_waiting, 0);
    assert_eq!(sweep.examined, 2);
    assert_eq!(sweep.unmatched, 2);
    assert!(sweep.fulfilled.is_empty());
}

/// A run the store refuses does not take the whole pass down.
///
/// `fulfil_from_inbound` refuses a run holding two open waits on ONE source —
/// correctly, because closing one is a coin flip. Propagating that would let a
/// single misconfigured run stop every other run's verification from landing,
/// which is a much larger failure than the one being reported.
#[test]
fn a_refused_run_is_carried_and_every_other_run_still_lands() {
    let (_tmp, runs, scope) = fixture();

    // One run waiting twice on the same source: the store's own coin-flip
    // refusal.
    let tangled = runs
        .open(
            &scope,
            "the tangled one",
            "https://portal.test/a",
            None,
            &[],
            "owner",
            at(0),
        )
        .expect("run");
    for description in ["the code", "the countersignature"] {
        runs.raise_expectation(
            &scope,
            &tangled.run_id,
            description,
            "shared@example.test",
            at(1),
        )
        .expect("wait");
    }
    // And a healthy run on its own source.
    let healthy = waiting_run(&runs, &scope, "the healthy one", INBOX);

    let inbox = Holding(vec![
        mail("msg-tangled", "shared@example.test", 2),
        mail("msg-healthy", INBOX, 3),
    ]);
    let sweep = sweep_run_inbox(&inbox, &runs, &scope, at(5)).expect("the pass survives");

    assert_eq!(
        sweep.refused.len(),
        1,
        "the tangled run is named, not swallowed"
    );
    assert_eq!(sweep.refused[0].run_id, tangled.run_id);
    assert!(
        !sweep.refused[0].reason.is_empty(),
        "the store's sentence says what to do about it"
    );
    assert_eq!(
        sweep.fulfilled.len(),
        1,
        "the healthy run's verification still landed"
    );
    assert_eq!(sweep.fulfilled[0].run_id, healthy);
}
