//! **The verification arrived. Which run was waiting for it?**
//!
//! Doc: `docs/plans/2026-08-07-opc-composable-work-modules.md` §5, which calls
//! this *"the primitive nobody has"* and *"the piece to prototype first"*:
//!
//! > the account-creation mail lands in Presto's own AgentMail; the agent must
//! > read its own inbox mid-flow, extract the code, and continue.
//!
//! [`crate::run_state`] has held up its half since it was built —
//! it records that a wait exists, matches an arriving event to it, and is
//! idempotent on a re-delivered one. Its module header says outright that it
//! *"never reads an inbox"*, and that refusal is what keeps a run usable
//! whatever channel its verification arrives on. What was missing is the other
//! half: **nothing read a mailbox and told it.** A run raised a wait and then
//! waited forever, because the only way to close one was a person noticing and
//! POSTing.
//!
//! # Which way the dependency runs
//!
//! A **coordinator**, like `delivery_hygiene`, `introductions` and
//! `retraction`. `run_state` stays ignorant of mailboxes; [`RunInbox`] is a
//! port this module owns, and a real mailbox implements three methods. Neither
//! side learns the other's shape.
//!
//! # It never settles a message, and that is not laziness
//!
//! `delivery_receipts`'s bounce bridge settles what it has handled, because a
//! bounce mailbox holds bounces and a handled one is finished. A run inbox is
//! an ORDINARY mailbox: it carries the verification code, and also every other
//! message that person receives. Settling on our way past would consume mail
//! that was never ours.
//!
//! So nothing is settled and nothing is remembered, and re-reading is safe
//! because [`fulfil_from_inbound`] is idempotent on `event_ref` — a
//! re-delivered message comes back [`InboundOutcome::AlreadyFulfilled`] and
//! writes nothing. The store's existing idempotency IS the cursor, which is
//! better than a cursor this module would have to keep honest.
//!
//! # Ambiguity across runs is refused, exactly as it is within one
//!
//! `fulfil_from_inbound` refuses when two open waits ON ONE RUN name the same
//! source, because closing one of them is a coin flip and the loser is closed
//! by an event that never satisfied it. Two waits on two DIFFERENT runs are the
//! same problem — one message satisfies one wait — and the store cannot see it,
//! because it is only ever asked about one run.
//!
//! So the ambiguity check lives here, before anything is fulfilled: a source
//! that more than one run is waiting on closes nothing, and is reported by name
//! so an owner can narrow the hints.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::run_state::{
    fulfil_from_inbound, InboundEvent, InboundOutcome, RunScope, RunStateStore,
};

/// A mailbox a run's verification might arrive in.
///
/// Deliberately not an AgentMail client, an IMAP session or a file watcher —
/// two methods over *"what are you holding"*, which every one of those can
/// satisfy and none of which this file has to learn.
///
/// There is no `settle`. See the module note: this is an ordinary mailbox and
/// consuming mail on our way past would take messages that were never ours.
pub trait RunInbox: Send + Sync {
    /// A short stable name for the report. Reported, never parsed.
    fn name(&self) -> &str;

    /// What the mailbox is holding, as events a run could be waiting for.
    ///
    /// An empty list means *"nothing to look at"* and is not an error. A
    /// mailbox that cannot be read **is** an error and must say so: an empty
    /// answer from a broken mailbox is indistinguishable from a quiet week,
    /// and the second reading is the one that leaves a run waiting forever on
    /// a verification that arrived.
    fn messages(&self, scope: &RunScope, now: DateTime<Utc>) -> Result<Vec<InboundEvent>>;
}

/// A source more than one run is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousSource {
    /// The hint as the MESSAGE carried it.
    ///
    /// Not the runs' spelling: the runs are indexed by the normalised form and
    /// their raw wording is not kept, and the two normalise to the same source
    /// by definition — that is why they collided. Reported unnormalised so it
    /// reads as something that actually arrived.
    pub source_hint: String,
    /// Every run waiting on it. Two is already too many.
    pub run_ids: Vec<String>,
    /// How many messages this pass held because of it.
    ///
    /// The source is reported once however many arrive on it — the instruction
    /// is *"narrow the hints"* and repeating it per message buries it — but the
    /// MESSAGES still have to be accounted for, or the buckets stop summing to
    /// `examined` and a reader is left asking where they went.
    pub messages: usize,
}

/// One wait this pass closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FulfilledWait {
    pub run_id: String,
    pub expectation_id: String,
    pub event_ref: String,
}

/// What one pass over the mailbox did.
///
/// Counts, not rates. *"Closed none"* over three messages and over three
/// hundred are different facts, and the second is the one that says the watch
/// is running and the runs are simply not being answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxSweep {
    /// Messages the mailbox offered.
    pub examined: usize,
    /// Runs holding at least one open wait when the pass began.
    pub runs_waiting: usize,
    pub fulfilled: Vec<FulfilledWait>,
    /// Messages that matched a run which had already recorded this exact
    /// event. Nothing was written. Counted apart from `unmatched` because
    /// *"already done"* and *"not for us"* are the same no-op and opposite
    /// facts.
    pub already_fulfilled: usize,
    /// Messages no run was waiting for. **The ordinary case**, not a failure:
    /// an inbox carries far more than one run's verification.
    pub unmatched: usize,
    /// Sources more than one run is waiting on. Nothing was closed for these.
    ///
    /// One entry per source, with its own message count — see
    /// [`AmbiguousSource::messages`] and [`InboxSweep::accounted_for`].
    pub ambiguous: Vec<AmbiguousSource>,
    /// Runs the store refused to fold a message into, and why.
    ///
    /// The refusal that actually happens is a run holding two open waits on one
    /// source — `fulfil_from_inbound`'s own coin-flip refusal. Carried rather
    /// than propagated: one misconfigured run must not stop every other run's
    /// verification from landing, and a refusal nobody can see is a run that
    /// waits forever for a reason nothing reports.
    pub refused: Vec<RefusedRun>,
}

/// A run the store would not fold a message into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedRun {
    pub run_id: String,
    /// The store's own sentence. Carried verbatim because it names what to do —
    /// narrowing the source hints — and a summary would lose that.
    pub reason: String,
}

impl InboxSweep {
    /// Every message this pass placed in a bucket.
    ///
    /// **This must equal [`Self::examined`]**, and the sweep's own test asserts
    /// it. Each message takes exactly one outcome — closed, already closed, not
    /// ours, held by an ambiguous source, or refused by its run — so a total
    /// that falls short means a message went somewhere nothing reports, which is
    /// how a verification that arrived becomes a run that waits forever.
    pub fn accounted_for(&self) -> usize {
        self.fulfilled.len()
            + self.already_fulfilled
            + self.unmatched
            + self.refused.len()
            + self
                .ambiguous
                .iter()
                .map(|held| held.messages)
                .sum::<usize>()
    }
}

/// Case- and whitespace-insensitive, matching what `fulfil_from_inbound` does
/// so the two cannot disagree about whether a source matched.
fn normalise(source_hint: &str) -> String {
    source_hint.trim().to_ascii_lowercase()
}

/// File one run under each distinct source it names.
///
/// Deduplicated per run: two waits on one source make the source ambiguous
/// WITHIN the run, which `fulfil_from_inbound` refuses on its own. Listing the
/// run twice here would make it look ambiguous ACROSS runs as well, and the
/// report would name one run as two.
fn index_sources(
    index: &mut BTreeMap<String, Vec<String>>,
    mut sources: Vec<String>,
    run_id: &str,
) {
    sources.sort();
    sources.dedup();
    for source in sources {
        index.entry(source).or_default().push(run_id.to_string());
    }
}

/// Read the mailbox and close whatever it answers.
///
/// # Failures are failures
///
/// An unreadable mailbox or run store propagates rather than folding to "closed
/// nothing". A run waiting on a verification that already arrived is invisible
/// from every other surface, so a pass that reported success over a store it
/// could not read would hide exactly the state this exists to find.
///
/// **One run refusing does not fail the pass.** `fulfil_from_inbound` refuses a
/// run holding two open waits on one source — correctly, because closing one is
/// a coin flip. Propagating that would let a single misconfigured run take the
/// whole watch down and stop every other run's verification from landing, which
/// is a much larger failure than the one being reported. Those come back in
/// [`InboxSweep::refused`], by run, so the run that needs narrowing is named.
pub fn sweep_run_inbox(
    inbox: &dyn RunInbox,
    store: &RunStateStore,
    scope: &RunScope,
    now: DateTime<Utc>,
) -> Result<InboxSweep> {
    let mut sweep = InboxSweep::default();

    // Which runs are waiting, and on what. Built once: the alternative is
    // asking every run about every message, which is the same answer and
    // quadratic in the two numbers most likely to grow.
    let mut waiting: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Waits already CLOSED, keyed by **(source, the event that closed it)**.
    //
    // Kept so a re-delivered message reads as *"already done"* rather than as
    // *"not for us"* — the store draws that distinction deliberately, and
    // collapsing it would count every re-read of a verification that arrived as
    // ordinary mail.
    //
    // Keyed by the event too, not by source alone. The first cut indexed the
    // source and then ASKED each run holding a closed wait on it, which is a
    // store read per candidate run per unmatched message. In the shape this
    // will actually be deployed in — one inbox, one `source_hint`, every
    // finished run filed under it — that is a full run load for every ordinary
    // email, growing with the number of runs ever completed. The event ref is
    // exactly what `fulfil_from_inbound`'s own replay check compares, so
    // indexing it answers the same question with a lookup and no read at all.
    let mut settled: BTreeMap<(String, String), String> = BTreeMap::new();
    for run in store.all_runs(scope)? {
        // A submitted run holds no open wait by construction, and
        // `expectations_awaiting` raises if the fold says otherwise. Reading
        // the run's own expectations directly keeps this pass out of that
        // argument: a terminal run contributes nothing to `waiting`, and its
        // closed waits still contribute to `settled`.
        let mut waits_on: Vec<String> = Vec::new();
        for expectation in &run.expectations {
            let source = normalise(&expectation.source_hint);
            match expectation.fulfilled.as_ref() {
                None => waits_on.push(source),
                // First writer wins on a collision, which cannot happen through
                // the store — one event closes one wait — and if it somehow did,
                // the answer is "already done" either way and only the reported
                // run id would differ.
                Some(fulfilment) => {
                    settled
                        .entry((source, fulfilment.event_ref.clone()))
                        .or_insert_with(|| run.run_id.clone());
                },
            }
        }
        if !waits_on.is_empty() {
            sweep.runs_waiting += 1;
        }
        index_sources(&mut waiting, waits_on, &run.run_id);
    }

    let messages = inbox.messages(scope, now)?;
    sweep.examined = messages.len();

    for message in messages {
        let source = normalise(&message.source_hint);
        let Some(run_ids) = waiting.get(&source) else {
            // Nothing is waiting on it. Before calling it ordinary mail, ask
            // whether a run already closed a wait with THIS EXACT event — a
            // mailbox re-delivering a verification that landed is a different
            // fact from a message that was never ours, and both are no-ops.
            //
            // A lookup, not a store read: the index carries the event ref, so
            // this answers the same question `fulfil_from_inbound`'s replay
            // check would, without loading a run.
            if settled.contains_key(&(source, message.event_ref.clone())) {
                sweep.already_fulfilled += 1;
            } else {
                sweep.unmatched += 1;
            }
            continue;
        };
        if run_ids.len() > 1 {
            // Reported once per source, not once per message: an owner narrows
            // the hints, and repeating the same instruction per message would
            // bury it.
            match sweep
                .ambiguous
                .iter_mut()
                .find(|held| normalise(&held.source_hint) == source)
            {
                Some(held) => held.messages += 1,
                None => sweep.ambiguous.push(AmbiguousSource {
                    source_hint: message.source_hint.clone(),
                    run_ids: run_ids.clone(),
                    messages: 1,
                }),
            }
            continue;
        }
        let run_id = &run_ids[0];

        match fulfil_from_inbound(store, scope, run_id, &message) {
            Ok(InboundOutcome::Fulfilled { expectation_id, .. }) => {
                sweep.fulfilled.push(FulfilledWait {
                    run_id: run_id.clone(),
                    expectation_id,
                    event_ref: message.event_ref.clone(),
                });
            },
            Ok(InboundOutcome::AlreadyFulfilled { .. }) => sweep.already_fulfilled += 1,
            // The run's own waits do not name this source after all — the map
            // was built from the same read, so this is a concurrent write
            // rather than a disagreement. Nothing was written either way.
            Ok(InboundOutcome::Unmatched) => sweep.unmatched += 1,
            // Carried, not propagated. See the note on `refused`: a run holding
            // two open waits on one source is refused by the store, and letting
            // that abort the pass would stop every OTHER run's verification
            // from landing.
            Err(error) => sweep.refused.push(RefusedRun {
                run_id: run_id.clone(),
                reason: format!("{error:#}"),
            }),
        }
    }

    Ok(sweep)
}

/// The first implementor: a directory of `.eml` files, committing to no
/// provider because every way a message reaches a host ends in a file.
pub mod maildir;

pub use maildir::MaildirRunInbox;

#[cfg(test)]
mod tests;
