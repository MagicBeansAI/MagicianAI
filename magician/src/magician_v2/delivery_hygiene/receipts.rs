//! Pulling receipts in, on a cadence, and carrying them the last mile.
//!
//! # What was still missing after the door was built
//!
//! [`delivery::intake::ReceiptIntake::admit`] is the one normalised door a
//! receipt comes through, and it is reachable from
//! `POST /api/magician/v2/delivery/receipts`. That is a **push** surface: it
//! works when somebody is holding a receipt and chooses to walk it through.
//! Nothing in this deployment holds one unprompted — no provider wired here
//! emits a delivery event stream — so on the push path alone an act is
//! reconciled only when a person notices a bounce and types it in.
//!
//! This module is the **pull**. Once per hygiene tick, per scope, it asks a
//! source what it is holding, walks each receipt through the same door, and
//! moves the outward disclosure off `dispatch_unknown` where the receipts prove
//! it. Then — on the same tick, immediately after — the sweep beside it carries
//! the resulting hard bounces and complaints to the suppression register, and
//! `outward_gate::contact_refusal` starts refusing that address.
//!
//! That chain is the point of the whole subsystem, and until it ran end to end
//! nothing proved it: every link was correct alone and the register stayed
//! empty.
//!
//! # One port, and it is the only thing here that is not already built
//!
//! [`ReceiptPuller`] is *"what have you got that I have not recorded?"*. A DSN
//! bridge reading a bounce mailbox, a webhook queue, a Kapso poller and an
//! operator's outbox are all the same shape from here.
//!
//! Nothing in this file imports a rail. The DSN implementation lives in
//! [`delivery_receipts::pull`], which already knows what a delivery-status
//! notification is; the dependency runs **specific → generic**, never back, so
//! a second rail is a second implementor and this file is not edited. That is
//! the same rule [`silence::rails_from_disclosures`](super::silence::rails_from_disclosures)
//! follows for rail attribution.
//!
//! [`delivery::intake::ReceiptIntake::admit`]: crate::magician_v2::delivery::intake::ReceiptIntake::admit
//! [`delivery_receipts::pull`]: crate::magician_v2::delivery_receipts::pull
//!
//! # One door, one route into suppression
//!
//! This pass does **not** call [`DeliveryLedger::reconcile`] and does **not**
//! touch the suppression register.
//!
//! - Every receipt goes through [`ReceiptIntake::admit`], so the act-must-have-left
//!   check, the attempt log and the ledger's order all apply exactly as they do
//!   to a receipt posted over HTTP. A worker with its own path into the ledger
//!   would be a second set of rules, and the two would drift.
//! - Suppression happens where it already happens, through
//!   [`sweep_delivery_into_suppression`](super::sweep_delivery_into_suppression).
//!   There is one route from a provider receipt to a register entry.
//!
//! # Fail closed, receipt by receipt
//!
//! - A receipt the door **refuses** — an act this scope never dispatched, one
//!   provider message claiming two identities, a changed body under a replayed
//!   id — is counted, **not settled**, and never retried in a different shape.
//!   The door's refusal is the answer.
//! - A source's own refusal (a bounce it could not correlate to a send) is
//!   carried through as [`ReceiptPass::uncorrelated`] rather than dropped. A
//!   bounce nobody could place is a broken send-side index, and it must not
//!   render as a quiet tick.
//! - `settle` is called **only after** the door returned, and only for a handle
//!   every one of whose receipts was admitted. A source told to forget a
//!   receipt this runtime never recorded has lost it for good; re-offering one
//!   already recorded costs a `Replayed` and nothing else.
//! - The whole pass is **carried, never raised**, by its caller. A source that
//!   cannot be read must not stop the bounces already in the ledger from
//!   reaching the register.
//!
//! # A disclosure moves only where the receipts prove it
//!
//! `dispatch_unknown` is not permission and its absence is not success, so this
//! pass never advances a disclosure on the *strongest* receipt. It asks
//! [`DeliveryLedger::reach`] over the act's own recorded audience and moves the
//! record only on a total answer — see [`disclosure_target`].
//!
//! # Counts, never rates
//!
//! [`ReceiptPass`] reports whole numbers and carries `examined` and `offered`
//! beside them. There is no `reconciliation_rate`: "94% reconciled" over
//! seventeen receipts and over seventeen thousand are different facts, and the
//! first is what a half-wired intake looks like in its first hour.

use std::collections::BTreeMap;
use std::fmt;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::magician_v2::agents::outward_gate::DispatchLog;
use crate::magician_v2::delivery::intake::{IntakeAttribution, ReceiptIntake, ReceiptSource};
use crate::magician_v2::delivery::{
    DeliveryLedger, DeliveryReceipt, DeliveryScope, DeliveryState, DispatchedAct, ReachReport,
    Reconciliation,
};
use crate::magician_v2::evidence::{OutwardActStatus, OutwardAssertionStore, OutwardScope};

/// Field separator for derived ids across this codebase. A caller string that
/// feeds one is refused if it holds this character, here as everywhere else.
const FIELD_SEP: char = '\u{1f}';

/// How the intake log attributes a receipt this worker pulled in.
///
/// A free string by the door's own design — it takes no opinion on how a caller
/// was proved. This one says *process*, not *person*, because a pulled receipt
/// was proved by nobody: it was read off a source this runtime was configured
/// with. An auditor reading `provider` beside this string knows it came in on
/// the cadence rather than through a hand at a keyboard.
const PULL_AUTHENTICATION: &str = "process:delivery-hygiene-pull";

/// One receipt a source is holding, already tied to the act it is about.
///
/// Correlating a receipt to an act is the **source's** job, and deliberately
/// so: it is the dangerous half, it is rail-specific, and getting it wrong
/// suppresses an address on the strength of somebody else's bounce. A DSN
/// source correlates on an identifier we minted; a webhook source has the id in
/// the payload. Neither guess is made here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledReceipt {
    /// The source's own handle for whatever it read this out of — a mail, a
    /// queue row, a file. Several receipts may share one: a report about three
    /// recipients is one document. The handle is settled once, and only when
    /// every receipt carrying it was admitted.
    pub handle: String,
    /// The act this receipt is about, as the source correlated it.
    pub act_ref: String,
    /// The receipt exactly as the source read it. Untouched here and untouched
    /// by the door.
    pub receipt: DeliveryReceipt,
    /// How the source tied it to the act, for the health line. Reported, never
    /// parsed.
    pub note: Option<String>,
}

/// What one source handed back, and what it could not.
///
/// The refusals are counted rather than dropped, because *"we read nine bounces
/// and could place none of them"* and *"there were no bounces"* are opposite
/// facts that produce the same `receipts.len()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PulledBatch {
    /// Documents, rows or messages the source looked at.
    pub examined: usize,
    /// Receipts it could place.
    pub receipts: Vec<PulledReceipt>,
    /// Reported facts it could **not** tie to an act. Never guessed at.
    pub uncorrelated: usize,
    /// Items that announced themselves as delivery reports and could not be
    /// read. Held, never settled: a human has to look.
    pub unreadable: usize,
}

/// Somewhere receipts come from.
///
/// The whole provider surface, in three methods. Implement it beside the
/// adapter that speaks the rail's language — never in this module, which stays
/// ignorant of every rail so that one nobody has built yet reconciles through
/// the same worker.
pub trait ReceiptPuller: Send + Sync {
    /// A short stable name for the health line — `dsn`, `kapso-webhook`. It is
    /// reported, never parsed.
    fn name(&self) -> &str;

    /// What this source holds that this runtime has not settled.
    ///
    /// An empty batch is a legitimate answer meaning *"nothing new"*. It is not
    /// an error and the caller does not read it as health: a source that always
    /// answers empty is indistinguishable from a quiet week until the silence
    /// watch beside it starts counting overdue acts.
    fn pull(&self, scope: &DeliveryScope, now: DateTime<Utc>) -> Result<PulledBatch>;

    /// Stop offering everything that came out of one handle.
    ///
    /// Called **only** after the door returned for every receipt carrying that
    /// handle, and only when every one of them was admitted. Settling first and
    /// crashing loses a receipt nothing will ever produce again; re-offering
    /// one already recorded costs one `Replayed`.
    fn settle(&self, scope: &DeliveryScope, handle: &str, now: DateTime<Utc>) -> Result<()>;
}

/// What one receipt pass did — counts, never rates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceiptPass {
    /// The source's name, so a health line can say which intake these numbers
    /// came from.
    pub source: String,
    /// Documents the source looked at.
    pub examined: usize,
    /// Receipts it placed and offered.
    pub offered: usize,
    /// Reported facts the source could not tie to an act. A rising count is a
    /// broken send-side index, not a quiet week.
    pub uncorrelated: usize,
    /// Items that announced themselves as delivery reports and could not be
    /// read.
    pub unreadable: usize,
    /// First thing this provider has said about this recipient on this act.
    pub opened: usize,
    /// A strictly stronger observation; the recipient's state moved.
    pub advanced: usize,
    /// The identical receipt again. One record, not two.
    pub replayed: usize,
    /// Recorded for audit and refused by the order — the state did not move.
    pub superseded: usize,
    /// Receipts the door refused. Not settled, so they stay visible.
    pub refused: usize,
    /// Acts whose ledger knowledge left `dispatch_unknown` on this pass — the
    /// number that answers *"is reconciliation actually happening"*.
    pub acts_left_dispatch_unknown: usize,
    /// Outward disclosures whose recorded status advanced on this pass.
    pub disclosures_advanced: usize,
    /// Of those, the ones that were sitting at `dispatch_unknown`.
    pub disclosures_left_dispatch_unknown: usize,
    /// Receipts whose disclosure was read and deliberately left where it was:
    /// already settled, corrected, retracted, still `prepared`, or the receipts
    /// so far do not justify a move.
    pub disclosures_unmoved: usize,
    /// Disclosures that could not be read or written. Counted apart from
    /// `refused`: the ledger holding a receipt the disclosure store could not
    /// be told about is a different fault from a receipt the door rejected.
    pub disclosure_failures: usize,
    /// Handles the source was told it may forget.
    pub settled: usize,
    /// Handles it could not be told about. Costs a re-offer, never truth.
    pub settle_failures: usize,
    /// The first fault of the pass, verbatim, so an operator has somewhere to
    /// start. Counts say how much went wrong; this says what.
    pub first_error: Option<String>,
}

impl ReceiptPass {
    /// Receipts the ledger now holds because of this pass, in any disposition.
    pub fn recorded(&self) -> usize {
        self.opened + self.advanced + self.replayed + self.superseded
    }

    /// Whether anything about this pass needs looking at.
    ///
    /// A pass that offered nothing is **not** clean by this measure — it is
    /// merely quiet, and the caller decides what quiet is worth. What is never
    /// clean is a reported fact nobody could place or a receipt the door
    /// refused.
    pub fn has_fault(&self) -> bool {
        self.uncorrelated > 0
            || self.unreadable > 0
            || self.refused > 0
            || self.disclosure_failures > 0
            || self.settle_failures > 0
    }

    fn note(&mut self, context: &str, error: impl fmt::Display) {
        if self.first_error.is_none() {
            self.first_error = Some(format!("{context}: {error}"));
        }
    }
}

/// Pull every receipt a source is holding and walk it through the door.
///
/// The named production entry point above this is
/// [`SuppressionSweepWorker::spawn_with_receipts`], whose tick calls it once
/// per configured scope immediately before
/// [`sweep_delivery_into_suppression`](super::sweep_delivery_into_suppression)
/// carries the resulting hard bounces and complaints to the register. That
/// order is deliberate: pulling after the sweep would leave a bounce waiting a
/// whole interval — fifteen minutes by default — before it could suppress
/// anybody, and the address stays sendable for every one of those minutes.
///
/// [`SuppressionSweepWorker::spawn_with_receipts`]: super::worker::SuppressionSweepWorker::spawn_with_receipts
///
/// # What propagates and what is counted
///
/// Only a source that cannot be **asked**, or a dispatch log that cannot be
/// **read**, propagates: without either there is nothing to iterate and the
/// pass has no result. Everything after that is per-receipt and is counted,
/// because one malformed receipt must not stop the nineteen behind it from
/// reaching the register.
pub fn pull_and_admit(
    door: &ReceiptIntake,
    dispatch_log: &DispatchLog,
    disclosures: &OutwardAssertionStore,
    scope: &DeliveryScope,
    puller: &dyn ReceiptPuller,
    now: DateTime<Utc>,
) -> Result<ReceiptPass> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator that \
             keeps derived ids' components apart, and a crafted scope could otherwise reconcile \
             a receipt into another owner's ledger"
        );
    }

    let batch = puller
        .pull(scope, now)
        .with_context(|| format!("asking receipt source `{}` what it holds", puller.name()))?;

    let mut pass = ReceiptPass {
        source: puller.name().to_string(),
        examined: batch.examined,
        offered: batch.receipts.len(),
        uncorrelated: batch.uncorrelated,
        unreadable: batch.unreadable,
        ..ReceiptPass::default()
    };
    if batch.receipts.is_empty() {
        return Ok(pass);
    }

    // Supplied, never discovered — the door's own rule, and the reason a
    // receipt can only ever speak about something this runtime actually sent.
    // Read once for the batch: it is the same list for every receipt in it, and
    // re-reading per receipt would let the answer change mid-pass.
    let dispatched: Vec<DispatchedAct> = dispatch_log
        .dispatched(scope)
        .context("reading what this scope recorded as dispatched")?;

    let outward_scope = OutwardScope::new(scope.principal.clone(), scope.workspace.clone());
    let attribution = IntakeAttribution {
        // A bridge speaking for a provider's own report, which is what a
        // delivery-status notification is. Not `Operator`: nobody typed it.
        source: ReceiptSource::Provider,
        actor: format!("worker:{}", puller.name()),
        authentication: PULL_AUTHENTICATION.to_string(),
    };

    // A handle may carry several receipts — one report about three recipients
    // is one document — and it may only be settled when every one of them was
    // admitted. Settling on the first would drop the other two.
    let mut outstanding: BTreeMap<String, usize> = BTreeMap::new();
    let mut admitted_by_handle: BTreeMap<String, usize> = BTreeMap::new();
    for pulled in &batch.receipts {
        *outstanding
            .entry(pulled.handle.trim().to_string())
            .or_insert(0) += 1;
    }

    for pulled in &batch.receipts {
        let handle = pulled.handle.trim().to_string();
        if handle.is_empty() {
            pass.refused += 1;
            pass.note(
                "a pulled receipt",
                "carries no handle, so it could never be settled and would be re-offered forever",
            );
            continue;
        }

        // Read BEFORE the write, so "this act left `dispatch_unknown`" is a
        // transition somebody observed rather than a state somebody inferred.
        let before = match door.ledger().state_of(scope, &pulled.act_ref) {
            Ok(before) => before,
            Err(error) => {
                pass.refused += 1;
                pass.note("reading the ledger before a receipt", format!("{error:#}"));
                continue;
            },
        };

        let admitted = match door.admit(
            scope,
            &dispatched,
            &pulled.act_ref,
            &pulled.receipt,
            &attribution,
            now,
        ) {
            Ok(admitted) => admitted,
            Err(error) => {
                pass.refused += 1;
                pass.note("admitting a pulled receipt", format!("{error:#}"));
                continue;
            },
        };

        match admitted.outcome.disposition {
            Reconciliation::Opened => pass.opened += 1,
            Reconciliation::Advanced { .. } => pass.advanced += 1,
            Reconciliation::Replayed => pass.replayed += 1,
            Reconciliation::Superseded { .. } => pass.superseded += 1,
        }
        if !before.is_reconciled() {
            pass.acts_left_dispatch_unknown += 1;
        }
        *admitted_by_handle.entry(handle).or_insert(0) += 1;

        advance_disclosure(
            door.ledger(),
            disclosures,
            scope,
            &outward_scope,
            &pulled.act_ref,
            now,
            &mut pass,
        );
    }

    // Settled last, and only for a handle every one of whose receipts landed.
    for (handle, expected) in outstanding {
        if admitted_by_handle.get(&handle).copied().unwrap_or(0) != expected {
            continue;
        }
        match puller.settle(scope, &handle, now) {
            Ok(()) => pass.settled += 1,
            Err(error) => {
                pass.settle_failures += 1;
                pass.note(
                    "settling a recorded receipt with its source",
                    format!("{error:#}"),
                );
            },
        }
    }

    Ok(pass)
}

/// Move the outward disclosure to whatever the act's receipts now prove — or
/// leave it exactly where it is.
///
/// Split out so the two decisions are separable and separately testable: what
/// the receipts prove ([`disclosure_target`]) and whether the record may be
/// moved there ([`may_write`]).
fn advance_disclosure(
    ledger: &DeliveryLedger,
    disclosures: &OutwardAssertionStore,
    scope: &DeliveryScope,
    outward_scope: &OutwardScope,
    act_ref: &str,
    now: DateTime<Utc>,
    pass: &mut ReceiptPass,
) {
    let act = match disclosures.load_act(outward_scope, act_ref) {
        Ok(Some(act)) => act,
        // The ledger holds a receipt for an act with no disclosure. Counted
        // rather than invented: this module never creates a disclosure, because
        // a disclosure asserts that something was said to somebody and only the
        // sender knows that.
        Ok(None) => {
            pass.disclosures_unmoved += 1;
            return;
        },
        Err(error) => {
            pass.disclosure_failures += 1;
            pass.note(
                "reading the outward disclosure for a receipt",
                format!("{error:#}"),
            );
            return;
        },
    };

    // The audience the act itself recorded, never the identities the receipts
    // happen to mention. Receipts exist only for people the provider said
    // something about, so deriving the audience from them would make everybody
    // the provider stayed silent about disappear — and that silence is exactly
    // what must keep the record at `dispatch_unknown`.
    if act.intended_audience.is_empty() {
        pass.disclosures_unmoved += 1;
        return;
    }

    let report = match ledger.reach(scope, act_ref, &act.intended_audience) {
        Ok(report) => report,
        Err(error) => {
            pass.disclosure_failures += 1;
            pass.note("asking who a reconciled act reached", format!("{error:#}"));
            return;
        },
    };

    let Some(target) = disclosure_target(&report) else {
        pass.disclosures_unmoved += 1;
        return;
    };
    if !may_write(act.status, target) {
        pass.disclosures_unmoved += 1;
        return;
    }

    let at = now.to_rfc3339();
    let wrote = match target {
        OutwardActStatus::Delivered => disclosures.record_delivered(outward_scope, act_ref, &at),
        OutwardActStatus::Failed => disclosures.mark_failed(
            outward_scope,
            act_ref,
            "every recipient's provider receipt is terminal and none is an arrival: this act \
             reached nobody",
            &at,
        ),
        // `disclosure_target` returns only the two above. The arm exists so a
        // third target added there fails loudly here rather than silently
        // writing nothing.
        other => Err(anyhow::anyhow!(
            "`{}` is not a status a provider receipt may move a disclosure to",
            other.as_str()
        )),
    };

    match wrote {
        Ok(()) => {
            pass.disclosures_advanced += 1;
            if act.status == OutwardActStatus::DispatchUnknown {
                pass.disclosures_left_dispatch_unknown += 1;
            }
        },
        Err(error) => {
            pass.disclosure_failures += 1;
            pass.note("advancing an outward disclosure", format!("{error:#}"));
        },
    }
}

/// What the act's receipts, taken together, prove about the disclosure.
///
/// `None` means *"not enough to move it"*, which is the answer far more often
/// than not and is never a failure.
///
/// # Why arrival is checked first, and alone
///
/// The ledger's own fold takes the **strongest** observation, because a
/// complaint anywhere is the act's complaint. That operator is right for "what
/// is the worst thing that happened" and wrong for "did this act tell anybody".
/// An act to two people where one hard-bounced and one was delivered folds to
/// `hard_bounce`, and marking the disclosure `failed` on that would say the act
/// told nobody — erasing an active disclosure and every correction obligation
/// resting on it, for somebody who definitely read it.
///
/// # And why silence outranks every failure
///
/// A single `unobserved` recipient holds the whole record at
/// `dispatch_unknown`, however bad the others look. `dispatch_unknown` is
/// neither evidence of a send nor of a non-send, and settling it on a partial
/// answer is the absence-becomes-a-positive move this whole subsystem exists to
/// refuse.
///
/// # Acceptance is not this function's business
///
/// A bare `Accepted` returns `None`. The send path already moves an act to
/// `provider_accepted` the moment it captures a provider message id, so a
/// receipt saying the same thing has nothing to add — and re-recording it would
/// append a fresh transition on every pass.
pub fn disclosure_target(report: &ReachReport) -> Option<OutwardActStatus> {
    // Somebody received it. That is a fact about the act and nothing later
    // takes it back.
    if !report.reached.is_empty() {
        return Some(OutwardActStatus::Delivered);
    }
    // Somebody is still unheard-of. The question stays open.
    if !report.unobserved.is_empty() {
        return None;
    }
    // Nobody was checked. `ReachReport::everyone_reached` refuses this shape
    // for the same reason: a verdict over an empty audience is vacuous.
    if report.fell_short.is_empty() {
        return None;
    }

    if report.fell_short.iter().all(|(_, state)| {
        matches!(
            state,
            DeliveryState::Bounced { hard: true } | DeliveryState::Failed
        )
    }) {
        return Some(OutwardActStatus::Failed);
    }
    // What is left includes at least one acceptance or one soft bounce. An
    // acceptance is not delivery and a soft bounce is transient — a mailbox
    // that was full on Tuesday may still receive this on Wednesday — so
    // nothing is settled and nothing is claimed.
    None
}

/// Whether a receipt may move a disclosure that currently reads `current`.
///
/// Terminal states never resurrect, and this is where that rule is enforced for
/// the disclosure record specifically — the ledger enforces its own separately.
///
/// - `Prepared` is never touched. Nothing left, so a receipt for it is a
///   contradiction somebody must look at, not a state to overwrite. It is also
///   where a **captured** act rests, and a rehearsal must never acquire a real
///   delivery.
/// - `Delivered` and `Failed` are settled. A late hard bounce on a delivered
///   act does not un-tell the person who read it; the ledger still records the
///   bounce and the suppression register still acts on it.
/// - `Corrected` and `Retracted` are successors' business. A delivery receipt
///   has no authority over a correction.
/// - `ProviderAccepted` may move to a terminal answer. The send path puts an
///   act there when it captures a message id, which is precisely the act a
///   bounce arrives about.
pub fn may_write(current: OutwardActStatus, target: OutwardActStatus) -> bool {
    match current {
        OutwardActStatus::Prepared
        | OutwardActStatus::Delivered
        | OutwardActStatus::Failed
        | OutwardActStatus::Corrected
        | OutwardActStatus::Retracted => false,
        OutwardActStatus::Dispatching
        | OutwardActStatus::DispatchUnknown
        | OutwardActStatus::ProviderAccepted => {
            matches!(
                target,
                OutwardActStatus::Delivered | OutwardActStatus::Failed
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        reached: &[&str],
        fell_short: &[(&str, DeliveryState)],
        unobserved: &[&str],
    ) -> ReachReport {
        ReachReport {
            act_ref: "act-1".to_string(),
            checked: reached.len() + fell_short.len() + unobserved.len(),
            reached: reached.iter().map(|s| s.to_string()).collect(),
            fell_short: fell_short
                .iter()
                .map(|(id, state)| ((*id).to_string(), *state))
                .collect(),
            unobserved: unobserved.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A mixed act must stay `delivered`, never become `failed`.
    ///
    /// Pins the bug the obvious implementation has: folding the act's receipts
    /// with the ledger's own `dominant` operator makes `hard_bounce` win over
    /// `delivered`, which is right for "what is the worst thing that happened"
    /// and catastrophic here. Marking this act `failed` says it told nobody,
    /// which drops it out of `is_active_disclosure` and takes every correction
    /// obligation resting on it with it — for a recipient who definitely read
    /// it.
    #[test]
    fn one_bounce_does_not_fail_an_act_somebody_received() {
        let mixed = report(
            &["ada@x.test"],
            &[("dead@x.test", DeliveryState::Bounced { hard: true })],
            &[],
        );
        assert_eq!(
            disclosure_target(&mixed),
            Some(OutwardActStatus::Delivered),
            "an act one recipient read is a delivered act, whatever happened to the other"
        );
    }

    /// One unheard-of recipient holds the whole record at `dispatch_unknown`.
    ///
    /// The absence-becomes-a-positive move, in its exact shape: two of three
    /// recipients hard-bounced, and settling the act as `failed` on that would
    /// be a verdict about the third that no provider ever gave.
    #[test]
    fn a_single_silent_recipient_settles_nothing() {
        let partial = report(
            &[],
            &[
                ("dead@x.test", DeliveryState::Bounced { hard: true }),
                ("gone@x.test", DeliveryState::Failed),
            ],
            &["quiet@x.test"],
        );
        assert_eq!(
            disclosure_target(&partial),
            None,
            "an unobserved recipient is neither an arrival nor a failure"
        );
    }

    /// Every recipient terminally failed, so the act reached nobody.
    #[test]
    fn an_act_every_recipient_bounced_reached_nobody() {
        let dead = report(
            &[],
            &[
                ("dead@x.test", DeliveryState::Bounced { hard: true }),
                ("gone@x.test", DeliveryState::Failed),
            ],
            &[],
        );
        assert_eq!(disclosure_target(&dead), Some(OutwardActStatus::Failed));
    }

    /// A soft bounce settles nothing: a full mailbox on Tuesday is not a dead
    /// address.
    #[test]
    fn a_soft_bounce_leaves_the_question_open() {
        let transient = report(
            &[],
            &[
                ("dead@x.test", DeliveryState::Bounced { hard: true }),
                ("full@x.test", DeliveryState::Bounced { hard: false }),
            ],
            &[],
        );
        assert_eq!(disclosure_target(&transient), None);
    }

    /// A bare acceptance settles nothing.
    ///
    /// The provider took the message; nobody has said anybody received it. The
    /// send path already moved the act to `provider_accepted` when it captured
    /// the message id, so a receipt repeating that has nothing to add — and
    /// returning it here would append a fresh transition on every pass, for as
    /// long as the act is never delivered.
    #[test]
    fn a_bare_acceptance_settles_nothing() {
        let accepted = report(&[], &[("ada@x.test", DeliveryState::Accepted)], &[]);
        assert_eq!(disclosure_target(&accepted), None);
    }

    /// A report over nobody proves nothing.
    ///
    /// The vacuous pass, in the one place it could still get in: an act whose
    /// audience resolved to nothing would otherwise satisfy every `all()` below
    /// trivially and settle as `failed`.
    #[test]
    fn a_report_over_nobody_settles_nothing() {
        assert_eq!(disclosure_target(&report(&[], &[], &[])), None);
    }

    /// A settled disclosure never resurrects, in either direction.
    #[test]
    fn a_settled_disclosure_is_never_rewritten() {
        for target in [OutwardActStatus::Delivered, OutwardActStatus::Failed] {
            assert!(
                !may_write(OutwardActStatus::Delivered, target),
                "a delivered act must not be moved to {}",
                target.as_str()
            );
            assert!(
                !may_write(OutwardActStatus::Failed, target),
                "a failed act must not be moved to {}",
                target.as_str()
            );
            assert!(
                !may_write(OutwardActStatus::Corrected, target),
                "a corrected act is a successor's business, not a receipt's"
            );
            assert!(
                !may_write(OutwardActStatus::Retracted, target),
                "a retracted act is a successor's business, not a receipt's"
            );
            assert!(
                !may_write(OutwardActStatus::Prepared, target),
                "nothing left for a prepared act, so a receipt for it is a contradiction"
            );
        }
    }

    /// `dispatch_unknown` is the state this whole tier exists to move, and both
    /// terminal answers may move it.
    #[test]
    fn dispatch_unknown_may_move_to_either_terminal_answer() {
        for target in [OutwardActStatus::Delivered, OutwardActStatus::Failed] {
            assert!(
                may_write(OutwardActStatus::DispatchUnknown, target),
                "a receipt must be able to move `dispatch_unknown` to {}",
                target.as_str()
            );
            assert!(
                may_write(OutwardActStatus::ProviderAccepted, target),
                "the send path parks a captured act at `provider_accepted`, which is precisely \
                 the act a bounce arrives about"
            );
        }
        assert!(
            !may_write(
                OutwardActStatus::ProviderAccepted,
                OutwardActStatus::ProviderAccepted
            ),
            "an acceptance must not be re-recorded as a fresh transition on every pass"
        );
    }
}
