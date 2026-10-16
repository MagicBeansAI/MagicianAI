//! The durable run's first caller — Module C's adapters.
//!
//! Plan: `docs/plans/2026-08-07-opc-composable-work-modules.md` §5.
//!
//! [`super::store`] is complete and nothing constructs it. §5's *"the agent
//! must read its own inbox mid-flow, extract the code, and continue — this is
//! the primitive nobody has"* names a **loop**, and the store deliberately
//! owns only half of it: it records that a wait exists and which event ended
//! it, and it never reads an inbox. The other half — deciding which open wait
//! an arrived message belongs to, and refusing to guess when it cannot tell —
//! had no home at all. That decision is what this module owns:
//!
//! - [`expectations_awaiting`] — what the run is waiting for, in a shape an
//!   inbox reader can match against;
//! - [`fulfil_from_inbound`] — one arrived event, matched to one open wait, or
//!   matched to nothing at all;
//! - [`gaps_for_owner`] — the questions a person still has to answer, oldest
//!   first.
//!
//! # It still reads no inbox
//!
//! Every one of these takes what the caller already holds. Nothing here opens
//! a mailbox, a phone log or a queue, for the same reason the store does not:
//! a primitive that knew how to read one channel would only ever serve flows
//! on that channel, and §5's case is a form that may verify by mail today and
//! by SMS tomorrow.
//!
//! # Generic first: a grant portal is one consumer, not the subject
//!
//! The primitive is *"a long-lived undertaking is waiting on something that
//! arrives elsewhere, and on answers only a person can give"*. That is the
//! same shape for a compliance questionnaire, a vendor onboarding, a visa
//! application or a funding form. No vocabulary from any of them appears
//! below.
//!
//! # Fail closed, everywhere
//!
//! - **A run that cannot be loaded is an error, never an empty list.** *"No
//!   run"* answered as *"waiting on nothing"* would let a caller conclude a
//!   verification had already landed.
//! - **An unmatched inbound is not an error, and fulfils nothing.** An inbox
//!   carries vastly more than one run's verification code, so *"this is not
//!   for us"* is the ordinary case — but it is reported, never silently
//!   swallowed, and it never closes a wait.
//! - **An ambiguous inbound is refused.** Two open waits on one source and one
//!   arrived event is a coin flip, and the losing wait would be closed by an
//!   event that never satisfied it — *"an unverified verification"*, which is
//!   exactly what the store's own fulfilment refuses.
//! - **An identical replay resumes.** The same event ref re-delivered against
//!   the wait it already closed reports that, rather than either erroring or
//!   reading as a fresh unmatched message.
//! - **Terminal never resurrects.** A submitted run is waiting for nothing,
//!   and an open wait on one is refused as a contradiction rather than
//!   reported as live work.
//!
//! # No id is derived here
//!
//! Expectation ids and gap keys are the store's, derived from strings it
//! already refuses `U+001F` in at the write. Nothing in this module joins
//! caller text into an id, so nothing here restates that refusal — a check
//! placed where it is not load-bearing is a check callers route around.
//!
//! # The run id is a parameter
//!
//! [`RunStateStore`] addresses one run at a time — its ids are derived from
//! `(scope, purpose, resource_ref)` and it keeps no index of them — so every
//! function here takes the run it is about. A consumer holding several runs
//! calls these once per run.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};

use super::store::{RunScope, RunStateStore};
use super::types::{Expectation, Run};

// ── 1. What the run is waiting for ──────────────────────────────────────────

/// One open wait, in the shape an inbox reader matches against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitingExpectation {
    pub run_id: String,
    /// The handle [`fulfil_from_inbound`] and the store both address the wait
    /// by.
    pub expectation_id: String,
    /// What is being waited for, in the caller's words.
    pub description: String,
    /// Where the event will arrive. The **only** thing an inbound is matched
    /// on, so it is the field a reader keys its watch by.
    pub source_hint: String,
    pub raised_at: DateTime<Utc>,
    /// How long this has been waiting, at the instant asked.
    ///
    /// Derived from the clock on every read, never stored: a written-down
    /// "waiting for 3 days" needs somebody to keep it honest, and the one
    /// thing a run spanning sessions cannot rely on is the previous session
    /// having done that.
    pub waiting_for: Duration,
}

/// Everything the run is still waiting on, longest wait first.
///
/// Order is `raised_at` then `expectation_id`, so the list is deterministic
/// across reads and the oldest wait — the one most likely to have been missed
/// — is at the top.
///
/// Refusals:
///
/// - **the run does not exist.** An absent run is not a run waiting on
///   nothing; answering with an empty list would let a caller read a typo'd
///   run id as a verification that has already landed;
/// - **a wait raised after `now`.** The instant is the caller's, and one
///   ahead of a stored `raised_at` is a broken clock. Reporting a negative
///   wait as a wait would put an impossible duration in front of whoever is
///   deciding what to chase;
/// - **a submitted run holding an open wait.** The store's submit gate refuses
///   a run with one, so the combination is a fold contradicting a write rule,
///   and it is raised rather than quietly reported as live work.
///
/// A submitted run with no open wait yields an empty list: the record of what
/// went out does not resurrect into work.
pub fn expectations_awaiting(
    store: &RunStateStore,
    scope: &RunScope,
    run_id: &str,
    now: DateTime<Utc>,
) -> Result<Vec<AwaitingExpectation>> {
    let run = require_run(store, scope, run_id)?;
    let open: Vec<&Expectation> = run
        .expectations
        .iter()
        .filter(|held| held.is_open())
        .collect();

    if run.submission.is_some() {
        if !open.is_empty() {
            anyhow::bail!(
                "run `{run_id}` is submitted but still holds {} open expectation(s); the submit \
                 gate refuses a run that is waiting, so this fold contradicts the write rule \
                 that produced it and must not be read as live work",
                open.len()
            );
        }
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(open.len());
    for expectation in open {
        if expectation.raised_at > now {
            anyhow::bail!(
                "expectation `{}` on run `{run_id}` was raised at {} which is after the instant \
                 asked ({}); a wait cannot have started in the future, and reporting a negative \
                 wait would put an impossible duration in front of whoever decides what to chase",
                expectation.expectation_id,
                expectation.raised_at.to_rfc3339(),
                now.to_rfc3339()
            );
        }
        out.push(AwaitingExpectation {
            run_id: run.run_id.clone(),
            expectation_id: expectation.expectation_id.clone(),
            description: expectation.description.clone(),
            source_hint: expectation.source_hint.clone(),
            raised_at: expectation.raised_at,
            waiting_for: now - expectation.raised_at,
        });
    }
    out.sort_by(|left, right| {
        left.raised_at
            .cmp(&right.raised_at)
            .then_with(|| left.expectation_id.cmp(&right.expectation_id))
    });
    Ok(out)
}

// ── 2. What arrived ─────────────────────────────────────────────────────────

/// One event the caller extracted from wherever it watches.
///
/// The extraction is the caller's — this module never reads the channel. What
/// arrives here is the ref of the thing that landed and where it landed, which
/// is everything the match needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEvent {
    /// The event itself — a message ref, a call record. Becomes the run's
    /// proof that the wait was satisfied by something retrievable, so it can
    /// never be blank: *"a wait closed by nothing is an unverified
    /// verification"*.
    pub event_ref: String,
    /// Where it arrived, as the caller knows it — matched against an open
    /// expectation's `source_hint`, ignoring surrounding whitespace and ASCII
    /// case so a reader naming an inbox slightly differently still matches.
    pub source_hint: String,
    pub at: DateTime<Utc>,
}

/// What one inbound event did to the run.
#[derive(Debug, Clone)]
pub enum InboundOutcome {
    /// Nothing open on this run watches that source.
    ///
    /// **Not an error.** An inbox carries far more than one run's
    /// verification, and treating every unrelated message as a failure would
    /// make the loop unusable. Nothing was written.
    Unmatched,
    /// The wait this event closed, and the run as it now stands.
    Fulfilled {
        expectation_id: String,
        run: Box<Run>,
    },
    /// This exact event had already closed that wait — a re-read channel
    /// re-delivering what it delivered before. Nothing was written.
    ///
    /// Distinguished from [`Self::Unmatched`] so a caller can tell *"already
    /// done"* from *"not for us"*: both are no-ops, but only one means the
    /// verification arrived.
    AlreadyFulfilled {
        expectation_id: String,
        run: Box<Run>,
    },
}

/// Match an arrived event to an open wait and fulfil it.
///
/// The match is on `source_hint` alone, trimmed and ASCII-case-insensitive.
/// That is the field the store itself documents as *"where the event will
/// arrive"*, and it is the only thing an inbox reader reliably knows about a
/// message before anybody has decided what it means. Matching on the
/// description instead would need this module to read the message body, which
/// is the coupling the whole primitive is arranged to avoid.
///
/// Outcomes:
///
/// - **exactly one open wait on that source** — it is fulfilled with this
///   event, and the updated run comes back;
/// - **no open wait on that source, but a fulfilled one this exact event
///   closed** — [`InboundOutcome::AlreadyFulfilled`], nothing written. An
///   identical replay resumes;
/// - **no open wait on that source** — [`InboundOutcome::Unmatched`], nothing
///   written. This is the ordinary case, not a failure;
/// - **two or more open waits on that source** — refused. Closing one of them
///   would be a coin flip, and the loser is closed by an event that never
///   satisfied it. The caller narrows the source hints, which is the same
///   thing the store's own `(description, source_hint)` id already asks of it.
///
/// A submitted run has no open wait by construction, so it falls through to
/// the replay check and then to `Unmatched` — a terminal run absorbs nothing.
pub fn fulfil_from_inbound(
    store: &RunStateStore,
    scope: &RunScope,
    run_id: &str,
    inbound: &InboundEvent,
) -> Result<InboundOutcome> {
    if inbound.event_ref.trim().is_empty() {
        anyhow::bail!(
            "an inbound event must carry the ref of what arrived; a wait closed by nothing is \
             an unverified verification, and the ref is also what makes a re-delivered event a \
             no-op"
        );
    }
    if inbound.source_hint.trim().is_empty() {
        anyhow::bail!(
            "an inbound event must say where it arrived; with no source there is nothing to \
             match against, and matching everything would close whichever wait happened to be \
             first"
        );
    }
    let run = require_run(store, scope, run_id)?;
    let wanted = inbound.source_hint.trim();

    // Ids, not references: the run is moved into the outcome further down, and
    // a borrow held across that move would be the compiler's problem rather
    // than the reader's.
    let open: Vec<String> = run
        .expectations
        .iter()
        .filter(|held| held.is_open() && same_source(&held.source_hint, wanted))
        .map(|held| held.expectation_id.clone())
        .collect();

    match open.len() {
        0 => {},
        1 => {
            let expectation_id = open[0].clone();
            let updated = store
                .fulfill_expectation(
                    scope,
                    run_id,
                    &expectation_id,
                    inbound.event_ref.trim(),
                    inbound.at,
                )
                .with_context(|| {
                    format!(
                        "fulfilling expectation `{expectation_id}` on run `{run_id}` with event \
                         `{}`",
                        inbound.event_ref.trim()
                    )
                })?;
            return Ok(InboundOutcome::Fulfilled {
                expectation_id,
                run: Box::new(updated),
            });
        },
        several => anyhow::bail!(
            "{several} open expectations on run `{run_id}` watch `{wanted}`; which one this \
             event satisfies is ambiguous, and closing either would leave the other closed by \
             an event that never satisfied it — give the waits distinguishable source hints"
        ),
    }

    // No open wait. An identical replay of the event that already closed one is
    // reported as such; anything else is simply not ours.
    let replay = run
        .expectations
        .iter()
        .find(|held| {
            same_source(&held.source_hint, wanted)
                && held
                    .fulfilled
                    .as_ref()
                    .is_some_and(|done| done.event_ref == inbound.event_ref.trim())
        })
        .map(|held| held.expectation_id.clone());
    if let Some(expectation_id) = replay {
        return Ok(InboundOutcome::AlreadyFulfilled {
            expectation_id,
            run: Box::new(run),
        });
    }
    Ok(InboundOutcome::Unmatched)
}

// ── 3. What a person still owes ─────────────────────────────────────────────

/// One question waiting on a human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerGap {
    pub run_id: String,
    /// The declared field this question is blocking.
    pub field: String,
    /// The question, in the words the owner will be asked.
    pub question: String,
    pub raised_at: DateTime<Utc>,
    /// Whether answering this is what holds the run out of review.
    ///
    /// A gap on an optional field is real and stays visible — the reviewing
    /// person weighs it — but it does not block. A gap whose field cannot be
    /// found is reported as blocking: the store refuses to raise one, so its
    /// existence means the fold disagrees with the write rules, and unknown is
    /// never permission to call a run reviewable.
    pub blocks_review: bool,
}

/// Every open gap on the run, oldest first.
///
/// §5: *"a question the evidence store cannot ground becomes an owner-facing
/// gap, never synthesised text. The owner's reply is new evidence."* This is
/// that queue, in the order a person should work it: `raised_at`, then field,
/// then question, so two gaps raised in the same instant still come back in a
/// stable order.
///
/// **Resolved gaps are excluded** — this is the list of what is still owed,
/// and a resolved question re-surfacing would train the owner to skim it. The
/// full history stays on the run.
///
/// A submitted run yields an empty list: whatever it once asked, nothing on it
/// is owed any more. A missing run is refused, never answered as *"nothing is
/// owed"*.
pub fn gaps_for_owner(
    store: &RunStateStore,
    scope: &RunScope,
    run_id: &str,
) -> Result<Vec<OwnerGap>> {
    let run = require_run(store, scope, run_id)?;
    if run.submission.is_some() {
        return Ok(Vec::new());
    }
    let mut out: Vec<OwnerGap> = run
        .gaps
        .iter()
        .filter(|gap| gap.is_open())
        .map(|gap| OwnerGap {
            run_id: run.run_id.clone(),
            field: gap.field.clone(),
            question: gap.question.clone(),
            raised_at: gap.raised_at,
            // Fail closed on an undeclared field: `Run::is_ready_for_review`
            // reads a missing field as non-blocking because it is gating a
            // write, while this is telling a person what to answer — and a
            // question whose field vanished is exactly the one somebody needs
            // to look at.
            blocks_review: run.field(&gap.field).is_none_or(|held| held.required),
        })
        .collect();
    out.sort_by(|left, right| {
        left.raised_at
            .cmp(&right.raised_at)
            .then_with(|| left.field.cmp(&right.field))
            .then_with(|| left.question.cmp(&right.question))
    });
    Ok(out)
}

// ── Internals ───────────────────────────────────────────────────────────────

/// Load a run, or refuse.
///
/// The one place *"no such run"* is turned into an error rather than an empty
/// answer. Every reader here is telling somebody what is outstanding, and an
/// absent run reported as *"nothing outstanding"* is the vacuous truth that
/// would let a mistyped id read as a finished form.
fn require_run(store: &RunStateStore, scope: &RunScope, run_id: &str) -> Result<Run> {
    store
        .load(scope, run_id)
        .with_context(|| format!("loading run `{run_id}`"))?
        .with_context(|| {
            format!(
                "no run `{run_id}` in this scope; an absent run is not a run with nothing \
                 outstanding, and answering as though it were would read a mistyped id as a \
                 finished form"
            )
        })
}

/// Whether an arrived event's source names the same place as a wait's hint.
///
/// Trimmed and ASCII-case-insensitive, because *"where the event will arrive"*
/// is free text a person typed and *"Inbox"* and *"inbox"* are one place.
/// Nothing beyond case is normalised: two hints that differ by a word are two
/// places, and collapsing them would close a wait on the wrong channel.
fn same_source(hint: &str, wanted: &str) -> bool {
    hint.trim().eq_ignore_ascii_case(wanted)
}

#[cfg(test)]
mod tests {
    //! The inbox loop, as behaviour: what a run is waiting for, what an
    //! arrived event closes, and what it must never close.

    use chrono::{DateTime, Duration, TimeZone, Utc};

    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::audience::AudienceRef;

    use super::super::store::{RunScope, RunStateStore};
    use super::super::types::{Answer, Run, RunState};
    use super::{
        expectations_awaiting, fulfil_from_inbound, gaps_for_owner, InboundEvent, InboundOutcome,
    };

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn later(minutes: i64) -> DateTime<Utc> {
        now() + Duration::minutes(minutes)
    }

    fn fixture() -> (tempfile::TempDir, RunStateStore, RunScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runs = RunStateStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, runs, RunScope::new("anonymous", "default"))
    }

    fn form_fields() -> Vec<(String, bool)> {
        vec![
            ("legal_name".to_string(), true),
            ("press_kit".to_string(), false),
        ]
    }

    fn open_run(runs: &RunStateStore, scope: &RunScope) -> Run {
        runs.open(
            scope,
            "sbir phase one application",
            "portal:grants/form-7",
            Some(AudienceRef::account("grantor")),
            &form_fields(),
            "company-assistant",
            now(),
        )
        .expect("open")
    }

    fn arrived(event_ref: &str, source_hint: &str, at: DateTime<Utc>) -> InboundEvent {
        InboundEvent {
            event_ref: event_ref.to_string(),
            source_hint: source_hint.to_string(),
            at,
        }
    }

    /// What a run is waiting on is read from the run, with the wait's age
    /// derived from the clock at the instant asked — never a stored duration
    /// that a crashed session would have left stale.
    #[test]
    fn an_awaiting_run_reports_its_wait_with_the_age_derived_now() {
        let (_tmp, runs, scope) = fixture();
        let run = open_run(&runs, &scope);
        runs.raise_expectation(
            &scope,
            &run.run_id,
            "the six digit verification code",
            "inbox:grants@company.test",
            later(5),
        )
        .expect("raise");

        let waiting = expectations_awaiting(&runs, &scope, &run.run_id, later(65)).expect("read");

        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].description, "the six digit verification code");
        assert_eq!(waiting[0].source_hint, "inbox:grants@company.test");
        assert_eq!(waiting[0].raised_at, later(5));
        assert_eq!(waiting[0].waiting_for.num_minutes(), 60);
        assert_eq!(waiting[0].run_id, run.run_id);
    }

    /// The ordinary case the loop has to survive: an inbox carries far more
    /// than one run's verification. An event on a source nothing watches is
    /// reported as unmatched, is not an error, and — the part that matters —
    /// closes no wait at all.
    #[test]
    fn an_unmatched_inbound_fulfils_nothing() {
        let (_tmp, runs, scope) = fixture();
        let run = open_run(&runs, &scope);
        runs.raise_expectation(
            &scope,
            &run.run_id,
            "the six digit verification code",
            "inbox:grants@company.test",
            later(5),
        )
        .expect("raise");

        let outcome = fulfil_from_inbound(
            &runs,
            &scope,
            &run.run_id,
            &arrived("msg-99", "inbox:newsletters@company.test", later(20)),
        )
        .expect("an unrelated message is not a failure");

        assert!(matches!(outcome, InboundOutcome::Unmatched));
        let after = runs
            .load(&scope, &run.run_id)
            .expect("load")
            .expect("present");
        assert_eq!(after.state(), RunState::AwaitingExternal);
        assert_eq!(after.expectations.len(), 1);
        assert!(after.expectations[0].fulfilled.is_none());
    }

    /// The match closes exactly the wait that named the source, records the
    /// event that satisfied it, and lets the run out of `awaiting_external`.
    /// Case and surrounding whitespace do not make an inbox a different inbox.
    #[test]
    fn a_matching_inbound_closes_that_wait_and_cites_its_event() {
        let (_tmp, runs, scope) = fixture();
        let run = open_run(&runs, &scope);
        let expectation = runs
            .raise_expectation(
                &scope,
                &run.run_id,
                "the six digit verification code",
                "inbox:grants@company.test",
                later(5),
            )
            .expect("raise");

        let outcome = fulfil_from_inbound(
            &runs,
            &scope,
            &run.run_id,
            &arrived("msg-7", "  Inbox:Grants@Company.test ", later(20)),
        )
        .expect("match");

        let InboundOutcome::Fulfilled {
            expectation_id,
            run: fulfilled,
        } = outcome
        else {
            panic!("expected the wait to be fulfilled");
        };
        assert_eq!(expectation_id, expectation.expectation_id);
        let done = fulfilled.expectations[0]
            .fulfilled
            .as_ref()
            .expect("fulfilment");
        assert_eq!(done.event_ref, "msg-7");
        assert_eq!(done.at, later(20));
        assert_eq!(fulfilled.state(), RunState::Drafting);
    }

    /// A re-read channel re-delivers what it delivered before. The replay is
    /// reported as already-fulfilled rather than as a fresh unmatched message,
    /// so a caller can tell "the code arrived" from "not for us" — and the
    /// store's record of WHICH event satisfied the wait is untouched.
    #[test]
    fn a_redelivered_event_reports_already_fulfilled_and_writes_nothing() {
        let (_tmp, runs, scope) = fixture();
        let run = open_run(&runs, &scope);
        runs.raise_expectation(
            &scope,
            &run.run_id,
            "the six digit verification code",
            "inbox:grants@company.test",
            later(5),
        )
        .expect("raise");
        let event = arrived("msg-7", "inbox:grants@company.test", later(20));
        fulfil_from_inbound(&runs, &scope, &run.run_id, &event).expect("first delivery");

        let outcome = fulfil_from_inbound(&runs, &scope, &run.run_id, &event).expect("re-delivery");

        let InboundOutcome::AlreadyFulfilled {
            expectation_id,
            run: replayed,
        } = outcome
        else {
            panic!("expected a replay, not a fresh match");
        };
        assert!(!expectation_id.is_empty());
        assert_eq!(replayed.expectations.len(), 1);
        assert_eq!(
            replayed.expectations[0]
                .fulfilled
                .as_ref()
                .expect("fulfilment")
                .event_ref,
            "msg-7"
        );
    }

    /// Two open waits on one source and one arrived event is a coin flip: the
    /// wait that loses would be closed by an event that never satisfied it —
    /// an unverified verification. The refusal names both, and writes nothing.
    #[test]
    fn two_open_waits_on_one_source_refuse_rather_than_guess() {
        let (_tmp, runs, scope) = fixture();
        let run = open_run(&runs, &scope);
        runs.raise_expectation(
            &scope,
            &run.run_id,
            "the six digit verification code",
            "inbox:grants@company.test",
            later(5),
        )
        .expect("raise");
        runs.raise_expectation(
            &scope,
            &run.run_id,
            "the countersigned participation letter",
            "inbox:grants@company.test",
            later(6),
        )
        .expect("raise");

        let error = fulfil_from_inbound(
            &runs,
            &scope,
            &run.run_id,
            &arrived("msg-7", "inbox:grants@company.test", later(20)),
        )
        .expect_err("ambiguous");

        assert!(error.to_string().contains("2 open expectations"), "{error}");
        let after = runs
            .load(&scope, &run.run_id)
            .expect("load")
            .expect("present");
        assert!(after
            .expectations
            .iter()
            .all(|held| held.fulfilled.is_none()));
    }

    /// An absent run is not a run with nothing outstanding. Answering an
    /// unknown id with an empty list would let a mistyped run read as a
    /// finished form, so every reader here refuses instead.
    #[test]
    fn a_missing_run_is_refused_by_every_reader() {
        let (_tmp, runs, scope) = fixture();

        let waiting =
            expectations_awaiting(&runs, &scope, "run-nope", now()).expect_err("unknown run");
        assert!(
            waiting.to_string().contains("no run `run-nope`"),
            "{waiting}"
        );

        let gaps = gaps_for_owner(&runs, &scope, "run-nope").expect_err("unknown run");
        assert!(gaps.to_string().contains("no run `run-nope`"), "{gaps}");

        let inbound = fulfil_from_inbound(
            &runs,
            &scope,
            "run-nope",
            &arrived("msg-7", "inbox:grants@company.test", now()),
        )
        .expect_err("unknown run");
        assert!(
            inbound.to_string().contains("no run `run-nope`"),
            "{inbound}"
        );
    }

    /// The owner's queue is oldest first, resolved questions are gone from it,
    /// and each row says whether answering it is what holds the run out of
    /// review — a gap on an optional field is real but does not block.
    #[test]
    fn open_gaps_come_back_oldest_first_and_say_what_blocks_review() {
        let (_tmp, runs, scope) = fixture();
        let run = open_run(&runs, &scope);
        runs.raise_gap(
            &scope,
            &run.run_id,
            "press_kit",
            "which logo pack should go in?",
            later(30),
        )
        .expect("optional gap");
        runs.raise_gap(
            &scope,
            &run.run_id,
            "legal_name",
            "is the trading name or the registered name wanted?",
            later(10),
        )
        .expect("required gap");
        runs.raise_gap(
            &scope,
            &run.run_id,
            "legal_name",
            "which jurisdiction's register should we cite?",
            later(5),
        )
        .expect("second required gap");
        runs.resolve_gap(
            &scope,
            &run.run_id,
            "legal_name",
            "which jurisdiction's register should we cite?",
            &Answer {
                text: "the Delaware register".to_string(),
                evidence_refs: vec!["reply-3".to_string()],
            },
            "dana",
            later(40),
        )
        .expect("resolve");

        let owed = gaps_for_owner(&runs, &scope, &run.run_id).expect("read");

        assert_eq!(owed.len(), 2, "the resolved question is no longer owed");
        assert_eq!(
            owed[0].question,
            "is the trading name or the registered name wanted?"
        );
        assert_eq!(owed[0].field, "legal_name");
        assert_eq!(owed[0].raised_at, later(10));
        assert!(owed[0].blocks_review);
        assert_eq!(owed[1].question, "which logo pack should go in?");
        assert_eq!(owed[1].field, "press_kit");
        assert!(!owed[1].blocks_review, "an optional field does not block");
    }

    /// A clock that runs backwards would report a negative wait as a wait. The
    /// instant is the caller's, so a stored `raised_at` ahead of it is refused
    /// rather than turned into an impossible duration for whoever decides what
    /// to chase.
    #[test]
    fn a_wait_raised_after_the_instant_asked_is_refused() {
        let (_tmp, runs, scope) = fixture();
        let run = open_run(&runs, &scope);
        runs.raise_expectation(
            &scope,
            &run.run_id,
            "the six digit verification code",
            "inbox:grants@company.test",
            later(60),
        )
        .expect("raise");

        let error =
            expectations_awaiting(&runs, &scope, &run.run_id, later(10)).expect_err("clock skew");
        assert!(
            error.to_string().contains("after the instant asked"),
            "{error}"
        );
    }
}
