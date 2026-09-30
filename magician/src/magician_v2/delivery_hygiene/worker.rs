//! The cadence that keeps the suppression register from being vacuous.
//!
//! [`super::sweep_delivery_into_suppression`] is a sweep somebody has to call.
//! Nobody did: `SuppressionRegister::ingest` had no non-test caller, so the
//! register stayed empty on disk, and
//! [`contact_refusal`](crate::magician_v2::agents::outward_gate::contact_refusal)
//! — a genuinely fail-closed guard — screened every recipient against nothing
//! and cleared all of them. It would have kept clearing them after they bounced
//! and after they complained. This worker is the writer that ends that.
//!
//! # The named entry point
//!
//! [`SuppressionSweepWorker::spawn`], called from `magician-bin/src/main.rs`
//! beside the other long-lived workers, configured by
//! [`crate::config::DeliveryHygieneConfig`]. Cadence, gating and health follow
//! the shape `magician_v2::outcome_learning::worker` already uses: a configured interval,
//! `MissedTickBehavior::Skip` so a slow tick does not queue a burst behind
//! itself, a [`CancellationToken`] for shutdown, and a snapshot a health
//! endpoint can read.
//!
//! # The cursor, and why losing it is cheap
//!
//! Each tick reads the ledger from [`super::HygieneCursor`] and advances it
//! only after the ingest returns. `SuppressionRegister::ingest` is idempotent
//! per `(identity, reason, evidence)`, and this sweep derives its evidence ref
//! from the window it read — so a lost cursor makes the next tick re-read
//! ground it already covered, resume the rows it already wrote, and land on
//! exactly the register it would have had. **Losing the cursor costs work,
//! never truth.** That is what lets the cursor be advanced after the work
//! rather than before it, which is the ordering that cannot skip a signal.
//!
//! # A tick that swept no scopes is degraded, never idle
//!
//! *"We found nothing to suppress"* and *"nobody told us whose ledger to read"*
//! produce the same zero, and only one of them is healthy. A worker reporting
//! the second as success is how this subsystem got here: green everywhere, and
//! not one identity on the register.
//!
//! # A failing scope does not stop the others
//!
//! Each scope is swept independently and a failure is counted and logged rather
//! than aborting the tick — one tenant's unreadable ledger must not stop
//! another tenant's complaints from being recorded. The tick still ends
//! `degraded`, so a scope that fails every time stays visible.
//!
//! # The silence watch rides the same tick
//!
//! [`super::silence`] asks the other half of the question: not *"what did a
//! provider tell us"* but *"what did a provider never tell us, and for how
//! long"*. It runs here rather than in a worker of its own for two reasons.
//!
//! The first is honest: a new worker would need a new `spawn` call in
//! `magician-bin/src/main.rs`, and a subsystem that ships correct and unspawned
//! is this programme's signature failure. This worker is already started, from
//! a named entry point, over an explicit scope list, against the same ledger.
//!
//! The second is that they are one concern. `dispatch_unknown` is neither
//! evidence of a send nor of a non-send, and the only thing that ever converts
//! it into either is a receipt. The hygiene sweep consumes receipts; the
//! silence watch counts their absence. A rising unacknowledged count is the
//! **only** early warning that a provider integration has silently broken —
//! every send succeeding, nothing ever confirming — and without it the
//! difference between noticing in an hour and noticing in a month is nobody
//! happening to look.
//!
//! The watch **writes nothing**. It reads the dispatch log, reads the ledger,
//! and derives every number from the clock, so its failure is reported in its
//! own state and can never corrupt the register the sweep above fills. The two
//! states are kept apart for the same reason: an operator watching `state`
//! reads whether suppressions are being recorded, and one watching
//! `silence_state` reads whether anything ever comes back.
//!
//! # And the receipt intake rides it too
//!
//! [`super::receipts`] is the third question, and the one that makes the other
//! two mean anything: *"has anybody handed us a receipt, and did we put it
//! where the sweep can find it?"* Until it existed the ledger had no producer
//! at all, so the sweep above carried nothing because there was nothing to
//! carry, and the watch below counted silence that no code path could ever end.
//!
//! It runs **first** in each scope's pass, before `opened_at` is taken, so a
//! bounce pulled in on this tick is inside the window this tick sweeps rather
//! than waiting a full interval to suppress anybody. Its state is reported in
//! its own field for the same reason the watch's is: `no_source` and `idle`
//! must never render alike, because the first means every live send stays at
//! `dispatch_unknown` for good.
//!
//! # Generic
//!
//! A principal, a workspace, a clock. Nothing here knows what was sent or to
//! whom: the ledger reports what a provider said about an identity and the
//! register decides what that is worth. A second rail — messaging, push,
//! postal — reconciles into the same ledger and is swept by this same worker
//! with nothing in this file to edit. The silence watch is generic in the same
//! way: it counts by whatever rail the act's own record names, so a rail that
//! does not exist yet appears on its own line the first time it goes quiet.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tokio::{
    sync::RwLock,
    task::JoinHandle,
    time::{interval, Duration as TokioDuration},
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::DeliveryHygieneConfig;
use crate::magician_v2::agents::outward_gate::DispatchLog;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::intake::ReceiptIntake;
use crate::magician_v2::delivery::{DeliveryLedger, DeliveryScope};
use crate::magician_v2::evidence::{OutwardAssertionStore, OutwardScope};
use crate::magician_v2::suppression::{SuppressionRegister, SuppressionScope};

use super::receipts::{pull_and_admit, ReceiptPass, ReceiptPuller};
use super::silence::{attribute, rails_from_disclosures, scan_silence, SilenceReport, SilentAct};
use super::{sweep_delivery_into_suppression, HygieneCursor, HygieneSweep};

const LOG_TARGET: &str = "delivery_hygiene::sweep_worker";

/// The floor on the tick interval.
///
/// Provider webhooks land in seconds and bounces in minutes; there is nothing
/// this sweep can learn by running every second except how to hammer a
/// filesystem. Applied with `max`, so a misconfigured zero becomes this rather
/// than a spin.
const MIN_TICK_INTERVAL_SECS: u64 = 60;

/// One owner to sweep.
///
/// Both halves of the pair, so the ledger scope and the register scope are
/// built from one source and cannot disagree —
/// [`sweep_delivery_into_suppression`] refuses a crossed pair, and this shape
/// is why that refusal can never fire from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HygieneScope {
    pub principal: String,
    pub workspace: String,
}

/// What the last tick did — counts, never rates.
///
/// *"Four scopes swept, eleven signals considered, three newly suppressed,
/// eight already held"* is a sentence an operator can act on. A single
/// "hygiene rate" would hide which number moved and read as progress in either
/// direction.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SuppressionSweepHealthSnapshot {
    pub enabled: bool,
    pub paused: bool,
    pub state: String,
    pub tick_interval_secs: u64,
    pub last_tick_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub scopes_seen: usize,
    pub scopes_failed: usize,
    /// Signals the ledgers offered across every scope this tick.
    pub considered: usize,
    /// Identities newly written to a register this tick.
    pub suppressed: usize,
    /// Signals naming an identity already suppressed for that reason.
    pub already_held: usize,
    /// Signals the register could not read, and so did not act on.
    ///
    /// Reported so `suppressed + already_held + signals_unreadable` accounts for
    /// `considered`. A non-zero value here means mail may still be going to
    /// addresses the ledger already reported as bounced — the sweep is running,
    /// which is exactly why nobody would otherwise look.
    pub signals_unreadable: usize,
    /// Identities newly suppressed since this process started. The number an
    /// operator checks to answer *"has this worker ever done anything"* — the
    /// question nobody could answer for the twenty modules that shipped inert.
    pub suppressed_total: usize,

    // ── The silence watch ───────────────────────────────────────────────────
    //
    // Held apart from the fields above, and settled by its own function, so an
    // operator watching `state` reads whether suppressions are being recorded
    // and one watching `silence_state` reads whether anything ever comes back.
    // Folding them would make a rail that has gone quiet read as a failure of
    // the register, and vice versa.
    /// `disabled`, `no_dispatches`, `idle` or `degraded`. **Never** `idle` on a
    /// tick that found nothing to look at — see [`finalize_silence`].
    pub silence_state: String,
    pub silence_grace_hours: i64,
    pub silence_alert_overdue: usize,
    /// Scopes whose silence could not be read this tick. Counted apart from
    /// `scopes_failed`: the watch failing must not read as the register failing.
    pub silence_scopes_failed: usize,
    /// Scopes that have never recorded a dispatch. Not a clean bill of health —
    /// nothing was sent, so nothing can be said about delivery.
    pub scopes_with_no_dispatches: usize,
    /// Distinct acts recorded as dispatched across every scope. The denominator
    /// the counts below are unreadable without.
    pub dispatched_scanned: usize,
    /// Acts some provider has said *something* about. The number that answers
    /// *"has a receipt ever arrived"*.
    pub acknowledged: usize,
    /// Acts nothing has ever acknowledged, at any age.
    pub unacknowledged: usize,
    /// Of those, the ones silent for at least the grace period. **This is the
    /// early-warning number**: it climbs when a provider integration breaks and
    /// every send goes on succeeding.
    pub overdue: usize,
    /// Acts whose recorded dispatch is later than the clock. Counted apart from
    /// both, because a silence cannot be measured against an instant it precedes.
    pub dispatched_ahead_of_the_clock: usize,
    pub longest_silence_secs: Option<i64>,
    /// Overdue counts per rail, so the line that moved names the integration
    /// that broke. Includes the `unattributed` bucket whenever anything lands
    /// in it.
    pub overdue_by_rail: BTreeMap<String, usize>,
    /// The oldest overdue acts, oldest first, capped by `silence_named_acts`.
    /// Counts are the signal; these are what make it investigable.
    pub oldest_unacknowledged: Vec<SilentAct>,
    pub silence_last_error: Option<String>,

    // ── The receipt intake ──────────────────────────────────────────────────
    //
    // The third state, kept apart from the other two for the same reason they
    // are kept apart from each other. `state` says whether suppressions are
    // being recorded, `silence_state` says whether anything ever comes back,
    // and `receipts_state` says whether anything is even **trying** to bring it
    // back. Folding them would let "no intake is attached" render as "the
    // intake found nothing", which is the exact substitution this whole tier
    // exists to make impossible.
    /// `disabled`, `no_source`, `idle` or `degraded`. **Never** `idle` while no
    /// source is attached — see [`finalize_receipts`].
    pub receipts_state: String,
    /// The attached source's own name, for the health line. `None` is the
    /// state where every live send stays at `dispatch_unknown` forever.
    pub receipts_source: Option<String>,
    /// Documents the source looked at this tick, across every scope. The
    /// denominator the counts below are unreadable without: "placed none of
    /// them" over two messages and over two hundred are different facts.
    pub receipts_examined: usize,
    /// Receipts the source offered this tick, across every scope.
    pub receipts_offered: usize,
    /// Reported facts the source could not tie to any act this scope sent. A
    /// rising count is a broken send-side index, never a quiet mailbox.
    pub receipts_uncorrelated: usize,
    /// Items that announced themselves as delivery reports and could not be
    /// read. Held by their source for a person to look at, never discarded.
    pub receipts_unreadable: usize,
    /// Of those, the ones the ledger now holds — opened, advanced, replayed or
    /// superseded. A superseded receipt is recorded and refused by the order,
    /// which is still a receipt this runtime accounted for.
    pub receipts_recorded: usize,
    /// Receipts the intake door refused — an act this scope never dispatched,
    /// one provider message claiming two identities, a changed body under a
    /// replayed id. Not settled, so they are re-offered and stay visible.
    pub receipts_refused: usize,
    /// Receipts recorded that the source could not be told about. Costs a
    /// re-offer next tick, never truth.
    pub receipts_settle_failures: usize,
    /// **The number that answers "is reconciliation actually happening".** Acts
    /// whose ledger knowledge left `dispatch_unknown` on this tick.
    pub acts_left_dispatch_unknown: usize,
    /// The same, accumulated since this process started. The question nobody
    /// could answer for the twenty modules that shipped inert: *has this ever
    /// done anything at all?*
    pub acts_left_dispatch_unknown_total: usize,
    /// Outward disclosures whose recorded status advanced this tick.
    pub disclosures_advanced: usize,
    /// Of those, the ones that were sitting at `dispatch_unknown`.
    pub disclosures_left_dispatch_unknown: usize,
    /// Disclosures that could not be read or written after their receipt was
    /// recorded. Counted apart from `receipts_refused`: the ledger holding a
    /// receipt the disclosure store never heard about is its own fault.
    pub disclosure_failures: usize,
    /// Scopes whose intake could not be asked this tick. Counted apart from
    /// `scopes_failed` and `silence_scopes_failed` so an operator is sent to
    /// the right log.
    pub receipts_scopes_failed: usize,
    pub receipts_last_error: Option<String>,
}

const NO_RECEIPT_SOURCE_REASON: &str =
    "no receipt source is attached to this process, so nothing pulls a provider receipt back \
     and every live send stays at `dispatch_unknown` for good. The sweep below it can only \
     carry what the ledger holds, and nothing is writing to the ledger. This is not a healthy \
     state and is deliberately not reported as one.";

impl SuppressionSweepHealthSnapshot {
    fn configured(config: &DeliveryHygieneConfig) -> Self {
        let receipts_unwired = config.enabled && !config.paused;
        Self {
            enabled: config.enabled,
            paused: config.paused,
            state: if !config.enabled {
                "disabled"
            } else if config.paused {
                "paused"
            } else {
                "idle"
            }
            .to_string(),
            tick_interval_secs: config.tick_interval_secs,
            silence_state: if !config.enabled || config.paused || !config.silence_watch_enabled {
                "disabled"
            } else {
                "idle"
            }
            .to_string(),
            silence_grace_hours: config.silence_grace_hours,
            silence_alert_overdue: config.silence_alert_overdue,
            // `no_source` out of the box, and deliberately not `idle`. A build
            // with no intake attached records no provider receipt at all, and
            // saying so from the first snapshot is the difference between an
            // operator knowing the loop is open and an operator reading a
            // cheerful zero.
            receipts_state: if !config.enabled || config.paused {
                "disabled"
            } else {
                "no_source"
            }
            .to_string(),
            receipts_last_error: receipts_unwired.then(|| NO_RECEIPT_SOURCE_REASON.to_owned()),
            ..Self::default()
        }
    }

    fn begin_tick(&mut self, at: &DateTime<Utc>) {
        self.state = "running".to_string();
        self.last_tick_at = Some(at.to_rfc3339());
        self.last_error = None;
        self.scopes_seen = 0;
        self.scopes_failed = 0;
        self.considered = 0;
        self.suppressed = 0;
        self.already_held = 0;
        self.signals_unreadable = 0;
        // Every silence number is derived from the clock on the tick that reads
        // it, so carrying one forward would report last tick's outage as this
        // tick's. They are reset together, and `silence_state` is settled from
        // the reset counts rather than left where it was.
        self.silence_scopes_failed = 0;
        self.scopes_with_no_dispatches = 0;
        self.dispatched_scanned = 0;
        self.acknowledged = 0;
        self.unacknowledged = 0;
        self.overdue = 0;
        self.dispatched_ahead_of_the_clock = 0;
        self.longest_silence_secs = None;
        self.overdue_by_rail = BTreeMap::new();
        self.oldest_unacknowledged = Vec::new();
        // A watch refused at startup keeps the sentence that says why. Clearing
        // it every tick would leave a `disabled` state with no stated reason,
        // which is the one thing worse than not running: nobody could tell a
        // watch that was switched off from one whose settings were rejected.
        if self.silence_state != "disabled" {
            self.silence_last_error = None;
        }
        // Per-tick receipt counts reset; the running total does not. The total
        // is the only field that answers "has a receipt EVER been reconciled by
        // this process", and a tick-scoped zero beside it is what tells an
        // operator whether the intake has gone quiet or was never fed.
        self.receipts_examined = 0;
        self.receipts_offered = 0;
        self.receipts_recorded = 0;
        self.receipts_uncorrelated = 0;
        self.receipts_unreadable = 0;
        self.receipts_refused = 0;
        self.receipts_settle_failures = 0;
        self.acts_left_dispatch_unknown = 0;
        self.disclosures_advanced = 0;
        self.disclosures_left_dispatch_unknown = 0;
        self.disclosure_failures = 0;
        self.receipts_scopes_failed = 0;
        // A `disabled` or `no_source` intake keeps the sentence that says why.
        // Clearing it would leave a refusal with no stated reason, and nobody
        // could tell an intake that was switched off from one that was never
        // wired.
        if self.receipts_source.is_some() {
            self.receipts_last_error = None;
        }
    }

    fn absorb(&mut self, swept: &HygieneSweep) {
        self.considered += swept.considered;
        self.suppressed += swept.suppressed;
        self.already_held += swept.already_held;
        self.signals_unreadable += swept.unreadable;
        self.suppressed_total += swept.suppressed;
    }

    fn absorb_receipts(&mut self, pass: &ReceiptPass) {
        self.receipts_examined += pass.examined;
        self.receipts_offered += pass.offered;
        self.receipts_recorded += pass.recorded();
        self.receipts_uncorrelated += pass.uncorrelated;
        self.receipts_unreadable += pass.unreadable;
        self.receipts_refused += pass.refused;
        self.receipts_settle_failures += pass.settle_failures;
        self.acts_left_dispatch_unknown += pass.acts_left_dispatch_unknown;
        self.acts_left_dispatch_unknown_total += pass.acts_left_dispatch_unknown;
        self.disclosures_advanced += pass.disclosures_advanced;
        self.disclosures_left_dispatch_unknown += pass.disclosures_left_dispatch_unknown;
        self.disclosure_failures += pass.disclosure_failures;
        // The first fault of the tick wins, verbatim. Counts say how much went
        // wrong and this says what, so an operator has somewhere to start.
        if self.receipts_last_error.is_none() {
            self.receipts_last_error = pass.first_error.clone();
        }
    }

    fn absorb_silence(&mut self, report: &SilenceReport, named_acts: usize) {
        if !report.any_dispatch_recorded {
            self.scopes_with_no_dispatches += 1;
            return;
        }
        self.dispatched_scanned += report.scanned;
        self.acknowledged += report.acknowledged;
        self.unacknowledged += report.unacknowledged;
        self.overdue += report.overdue;
        self.dispatched_ahead_of_the_clock += report.dispatched_ahead_of_the_clock;
        if let Some(longest) = report.longest_silence_secs {
            self.longest_silence_secs = Some(
                self.longest_silence_secs
                    .map_or(longest, |held: i64| held.max(longest)),
            );
        }
        for rail in &report.by_rail {
            *self.overdue_by_rail.entry(rail.rail.clone()).or_insert(0) += rail.overdue;
        }
        // Oldest first across every scope, then capped — not capped per scope
        // and concatenated, which would let one noisy tenant crowd out an older
        // silence somewhere else.
        self.oldest_unacknowledged
            .extend(report.acts.iter().cloned());
        self.oldest_unacknowledged
            .sort_by(|left, right| left.dispatched_at.cmp(&right.dispatched_at));
        self.oldest_unacknowledged.truncate(named_acts);
    }
}

/// Settle a finished tick's state.
///
/// Split out and tested directly, because the interesting decision is which
/// zero means what. A tick that swept **no scopes at all** is `degraded`: an
/// unconfigured scope list and a quiet day produce the same counts, and only
/// one of them is a healthy system. A tick that swept scopes and suppressed
/// nobody is `idle` — that is a week with no bounces, which is the ordinary
/// case and the one we hope for.
fn finalize_tick(snapshot: &mut SuppressionSweepHealthSnapshot, completed_at: &DateTime<Utc>) {
    if snapshot.scopes_seen == 0 {
        snapshot.state = "degraded".to_string();
        snapshot.last_error = Some(
            "no scope was swept, so no bounce or complaint could have reached any register; an \
             unconfigured scope list and a quiet day produce the same counts"
                .to_string(),
        );
        return;
    }
    if snapshot.scopes_failed > 0 {
        snapshot.state = "degraded".to_string();
        return;
    }
    snapshot.state = "idle".to_string();
    snapshot.last_success_at = Some(completed_at.to_rfc3339());
}

/// Settle the silence watch's own state.
///
/// Its own function, and its own field, because every zero here means something
/// different and collapsing them is exactly how a subsystem stays broken behind
/// a green dashboard:
///
/// - **A scope whose silence could not be read** is `degraded`. An unreadable
///   dispatch log answering "nothing is unacknowledged" would manufacture the
///   confidence this watch exists to withhold.
/// - **No scope swept at all** is `degraded`, for the same reason the sweep
///   above is: an unconfigured scope list and a clean run produce identical
///   counts.
/// - **Every scope with no dispatches** is `no_dispatches` — deliberately not
///   `idle`. Nothing was sent, so nothing can be said about delivery, and a
///   quiet-looking `idle` here would be the vacuous pass wearing a state name.
/// - **Overdue at or above the configured floor** is `degraded`, naming the
///   counts and the worst rail. This is the whole signal: a send that nobody
///   ever acknowledges is invisible, and the number climbing is the only thing
///   that says a provider integration has silently stopped answering.
/// - Anything else is `idle`: acts were dispatched, receipts came back, and
///   nothing has been silent past its grace.
///
/// A `disabled` watch is left alone — a tick must never quietly re-enable it.
pub(crate) fn finalize_silence(snapshot: &mut SuppressionSweepHealthSnapshot) {
    if snapshot.silence_state == "disabled" {
        return;
    }
    if snapshot.silence_scopes_failed > 0 {
        snapshot.silence_state = "degraded".to_string();
        snapshot.silence_last_error = Some(format!(
            "{} scope(s) could not be read for unacknowledged dispatches; an unreadable log is \
             never an empty one",
            snapshot.silence_scopes_failed
        ));
        return;
    }
    if snapshot.scopes_seen == 0 {
        snapshot.silence_state = "degraded".to_string();
        snapshot.silence_last_error = Some(
            "no scope was looked at, so nothing could be found unacknowledged; an unconfigured \
             scope list and a delivered week produce the same counts"
                .to_string(),
        );
        return;
    }
    if snapshot.scopes_with_no_dispatches == snapshot.scopes_seen {
        snapshot.silence_state = "no_dispatches".to_string();
        snapshot.silence_last_error = Some(
            "no scope has recorded a dispatch, so nothing can be said about delivery; this is \
             not evidence that sending works"
                .to_string(),
        );
        return;
    }
    if snapshot.overdue >= snapshot.silence_alert_overdue {
        let worst = snapshot
            .overdue_by_rail
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
            .map(|(rail, count)| format!("{rail} ({count})"))
            .unwrap_or_else(|| "no rail could be named".to_string());
        snapshot.silence_state = "degraded".to_string();
        snapshot.silence_last_error = Some(format!(
            "{} of {} dispatched act(s) have been silent past the grace period and {} have never \
             been acknowledged at all; worst rail: {worst}. Read this beside `receipts_state`, \
             which is `{}`: with no source attached nothing records a provider receipt at all, \
             so this is what a rail that never answers looks like — and it is what a rail that \
             has silently STOPPED answering will look like too",
            snapshot.overdue,
            snapshot.dispatched_scanned,
            snapshot.unacknowledged,
            snapshot.receipts_state
        ));
        return;
    }
    snapshot.silence_state = "idle".to_string();
}

/// Settle the receipt intake's own state.
///
/// Its own function and its own field, because the zeroes here mean something
/// different again from the sweep's and the watch's:
///
/// - **No source attached** is `no_source`, never `idle`. It is the state this
///   build ships in until an adapter is wired, and it means every live send
///   stays at `dispatch_unknown` forever. A cheerful `idle` beside a zero
///   `receipts_offered` would say the intake ran and found nothing, which is
///   the substitution that kept twenty modules inert behind a green dashboard.
/// - **A scope whose intake could not be asked** is `degraded`. An unreadable
///   source answering "no receipts" would manufacture exactly the confidence a
///   receipt exists to withhold.
/// - **No scope swept at all** is `degraded`, for the same reason the two
///   finalizers above are.
/// - **A receipt nobody could place, or one the ledger refused,** is
///   `degraded`. An uncorrelated bounce means a provider is talking and
///   nothing recorded which act its message id belonged to, which is a broken
///   send path wearing a quiet count.
/// - Anything else is `idle`: an intake is attached, it was asked, and
///   everything it offered was accounted for. `receipts_offered: 0` under
///   `idle` is an honest quiet tick — and `acts_left_dispatch_unknown_total`
///   beside it says whether this process has ever reconciled anything at all.
///
/// A `disabled` intake is left alone — a tick must never quietly re-enable it.
pub(crate) fn finalize_receipts(snapshot: &mut SuppressionSweepHealthSnapshot) {
    if snapshot.receipts_state == "disabled" {
        return;
    }
    if snapshot.receipts_source.is_none() {
        snapshot.receipts_state = "no_source".to_string();
        snapshot.receipts_last_error = Some(NO_RECEIPT_SOURCE_REASON.to_owned());
        return;
    }
    if snapshot.receipts_scopes_failed > 0 {
        snapshot.receipts_state = "degraded".to_string();
        if snapshot.receipts_last_error.is_none() {
            snapshot.receipts_last_error = Some(format!(
                "{} scope(s) could not be asked for provider receipts; an unreadable source is \
                 never an empty one",
                snapshot.receipts_scopes_failed
            ));
        }
        return;
    }
    if snapshot.scopes_seen == 0 {
        snapshot.receipts_state = "degraded".to_string();
        snapshot.receipts_last_error = Some(
            "no scope was looked at, so no receipt could have been pulled; an unconfigured scope \
             list and an intake with nothing to offer produce the same counts"
                .to_string(),
        );
        return;
    }
    if snapshot.receipts_uncorrelated > 0
        || snapshot.receipts_unreadable > 0
        || snapshot.receipts_refused > 0
        || snapshot.disclosure_failures > 0
        || snapshot.receipts_settle_failures > 0
    {
        snapshot.receipts_state = "degraded".to_string();
        if snapshot.receipts_last_error.is_none() {
            snapshot.receipts_last_error = Some(format!(
                "across {} document(s) examined and {} receipt(s) offered: {} reported fact(s) \
                 could not be tied to any act this scope sent, {} report(s) announced themselves \
                 and could not be read, {} receipt(s) were refused at the door, {} could not be \
                 carried onto their disclosure and {} could not be settled with their source. \
                 Nothing was dropped — every one of them is still with its source",
                snapshot.receipts_examined,
                snapshot.receipts_offered,
                snapshot.receipts_uncorrelated,
                snapshot.receipts_unreadable,
                snapshot.receipts_refused,
                snapshot.disclosure_failures,
                snapshot.receipts_settle_failures
            ));
        }
        return;
    }
    snapshot.receipts_state = "idle".to_string();
}

#[derive(Clone)]
pub struct SuppressionSweepHealth {
    snapshot: Arc<RwLock<SuppressionSweepHealthSnapshot>>,
}

impl SuppressionSweepHealth {
    pub fn new(config: &DeliveryHygieneConfig) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(SuppressionSweepHealthSnapshot::configured(
                config,
            ))),
        }
    }

    pub async fn snapshot(&self) -> SuppressionSweepHealthSnapshot {
        self.snapshot.read().await.clone()
    }

    fn mark_degraded(&self, message: impl Into<String>) {
        if let Ok(mut snapshot) = self.snapshot.try_write() {
            snapshot.state = "degraded".to_string();
            snapshot.last_error = Some(message.into());
        }
    }

    /// Decline to run the silence watch, visibly.
    ///
    /// `disabled` with a stated reason, not `idle` with none: a watch that was
    /// refused at startup and a watch that ran and found nothing must never
    /// render alike, or an unusable setting reads as a delivered week.
    fn refuse_silence_watch(&self, message: impl Into<String>) {
        if let Ok(mut snapshot) = self.snapshot.try_write() {
            snapshot.silence_state = "disabled".to_string();
            snapshot.silence_last_error = Some(message.into());
        }
    }

    /// Record that a receipt source is attached, by name.
    ///
    /// The name is what makes the health line readable — *"dsn offered 4,
    /// recorded 4"* rather than *"something offered 4"* — and its presence is
    /// what [`finalize_receipts`] reads to tell an attached intake from an
    /// absent one. `disabled` is left alone: a config that switched the whole
    /// worker off must not be quietly re-described because a source was passed.
    fn attach_receipt_source(&self, name: &str) {
        if let Ok(mut snapshot) = self.snapshot.try_write() {
            snapshot.receipts_source = Some(name.to_string());
            if snapshot.receipts_state != "disabled" {
                snapshot.receipts_state = "idle".to_string();
                snapshot.receipts_last_error = None;
            }
        }
    }
}

/// What the silence watch needs for one pass, or nothing when it is off.
#[derive(Debug, Clone, Copy)]
struct SilenceWatch {
    grace: Duration,
    named_acts: usize,
}

/// One scope's pass: the receipt intake, the register sweep, and the silence
/// read that rides with them.
struct ScopePass {
    /// `None` when no intake is attached. `Some(Err)` when the source could not
    /// be asked — carried rather than raised, so a broken intake never stops
    /// the bounces already in the ledger from reaching the register.
    receipts: Option<Result<ReceiptPass, String>>,
    swept: HygieneSweep,
    /// `None` when the watch is off. `Some(Err)` when it ran and could not read
    /// — carried rather than raised, so a silence-read failure never stops a
    /// tenant's complaints from reaching the register.
    silence: Option<Result<SilenceReport, String>>,
}

/// The periodic delivery-hygiene sweep.
pub struct SuppressionSweepWorker {
    handle: Option<JoinHandle<()>>,
    cancel: CancellationToken,
    health: SuppressionSweepHealth,
}

impl SuppressionSweepWorker {
    /// Start the worker, or decline to and say why.
    ///
    /// Declines — visibly, through the health snapshot — when the config is off
    /// or paused, and when it is on but names no scope. The last one matters:
    /// a worker started with an empty scope list would tick forever, sweep
    /// nothing, and report zeros that are indistinguishable from a clean
    /// register. Refusing at startup puts the fault where somebody reads it.
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        config: DeliveryHygieneConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self::spawn_with_receipts(workspace_layout, config, cancel, None)
    }

    /// Start the worker with a provider-receipt intake attached.
    ///
    /// **This is the named entry point for tier 4.** Everything below the
    /// socket — [`pull_and_admit`], the intake door, the ledger write, the
    /// disclosure moving off `dispatch_unknown`, the hard bounce reaching the
    /// suppression register, and `contact_refusal` then refusing the next send
    /// to that address — is reachable from this one call and from nothing else.
    ///
    /// [`spawn`](Self::spawn) is this with `None`, which is what
    /// `magician-bin/src/main.rs` calls today.
    /// [`DsnPuller`](crate::magician_v2::delivery_receipts::pull::DsnPuller)
    /// implements the port and is ready to be passed here; what it needs is a
    /// [`BounceMailbox`](crate::magician_v2::delivery_receipts::pull::BounceMailbox),
    /// and this build has no in-process mailbox reader to give it. A worker
    /// started with a source it does not have would be worse than one that says
    /// so, and with `None` the health snapshot reports
    /// `receipts_state: "no_source"` on every tick with the sentence that
    /// explains what that costs — never `idle`, and never a zero that reads
    /// like a quiet week.
    pub fn spawn_with_receipts(
        workspace_layout: ArtifactV2Workspace,
        config: DeliveryHygieneConfig,
        cancel: CancellationToken,
        receipts: Option<Arc<dyn ReceiptPuller>>,
    ) -> Self {
        let health = SuppressionSweepHealth::new(&config);
        if let Some(puller) = receipts.as_ref() {
            health.attach_receipt_source(puller.name());
        }
        if !config.enabled || config.paused {
            info!(
                target: LOG_TARGET,
                enabled = config.enabled,
                paused = config.paused,
                "delivery-hygiene sweep not started by configuration"
            );
            return Self {
                handle: None,
                cancel,
                health,
            };
        }

        let scopes: Vec<HygieneScope> = config
            .scopes
            .iter()
            .map(|scope| HygieneScope {
                principal: scope.principal.clone(),
                workspace: scope.workspace.clone(),
            })
            .collect();
        if scopes.is_empty() {
            health.mark_degraded(
                "the delivery-hygiene sweep is enabled but names no scope, so no ledger would \
                 ever be read; nothing was started",
            );
            warn!(target: LOG_TARGET, "delivery-hygiene sweep enabled with no scopes");
            return Self {
                handle: None,
                cancel,
                health,
            };
        }

        // The silence watch's own gate. Refused settings decline the watch and
        // say so; they never substitute a window nobody chose, because the
        // number this watch reports is the one somebody will act on.
        let watch = if !config.silence_watch_enabled {
            None
        } else if config.silence_grace_hours <= 0 {
            health.refuse_silence_watch(format!(
                "`silence_grace_hours` is {}, and an act is unacknowledged the instant it \
                 leaves: a grace at or below zero makes every live send overdue and a signal \
                 that is always on says nothing. The watch was not started.",
                config.silence_grace_hours
            ));
            warn!(
                target: LOG_TARGET,
                silence_grace_hours = config.silence_grace_hours,
                "silence watch enabled with an unusable grace period"
            );
            None
        } else if config.silence_alert_overdue == 0 {
            health.refuse_silence_watch(
                "`silence_alert_overdue` is 0, which reports every tick as degraded whatever \
                 the ledger says — including a tick where every send was acknowledged. A \
                 signal that is always on is one nobody reads, and this one is the only early \
                 warning a broken rail produces. The watch was not started.",
            );
            warn!(target: LOG_TARGET, "silence watch enabled with an always-on alert floor");
            None
        } else if config.silence_named_acts == 0 {
            health.refuse_silence_watch(
                "`silence_named_acts` is 0, so the watch could count acts and name none of \
                 them; a report that names nothing is what nobody investigates. The watch was \
                 not started.",
            );
            warn!(target: LOG_TARGET, "silence watch enabled naming no acts");
            None
        } else {
            Some(SilenceWatch {
                grace: Duration::hours(config.silence_grace_hours),
                named_acts: config.silence_named_acts,
            })
        };

        let worker_cancel = cancel.clone();
        let worker_health = health.clone();
        let handle = tokio::spawn(async move {
            let cadence =
                TokioDuration::from_secs(config.tick_interval_secs.max(MIN_TICK_INTERVAL_SECS));
            let mut ticker = interval(cadence);
            // A slow tick must not queue a burst behind itself: the sweep is
            // idempotent, so a skipped tick costs nothing and a stampede of
            // catch-up ticks costs a filesystem.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = worker_cancel.cancelled() => break,
                    _ = ticker.tick() => {},
                }

                {
                    let mut snapshot = worker_health.snapshot.write().await;
                    snapshot.begin_tick(&Utc::now());
                }

                for scope in &scopes {
                    let layout = workspace_layout.clone();
                    let scope = scope.clone();
                    let puller = receipts.clone();
                    let outcome = tokio::task::spawn_blocking(move || {
                        sweep_one(&layout, &scope, watch, puller.as_deref())
                    })
                    .await;
                    let mut snapshot = worker_health.snapshot.write().await;
                    snapshot.scopes_seen += 1;
                    match outcome {
                        Ok(Ok(pass)) => {
                            match pass.receipts {
                                Some(Ok(receipts)) => snapshot.absorb_receipts(&receipts),
                                // Counted apart from `scopes_failed`: an intake
                                // that could not be asked is not a register
                                // that could not be written, and reporting one
                                // as the other sends somebody to the wrong log.
                                Some(Err(error)) => {
                                    snapshot.receipts_scopes_failed += 1;
                                    if snapshot.receipts_last_error.is_none() {
                                        snapshot.receipts_last_error = Some(error.clone());
                                    }
                                    warn!(target: LOG_TARGET, %error, "receipt intake failed for one scope");
                                },
                                None => {},
                            }
                            snapshot.absorb(&pass.swept);
                            match pass.silence {
                                Some(Ok(report)) => {
                                    let named = watch.map_or(0, |watch| watch.named_acts);
                                    snapshot.absorb_silence(&report, named);
                                },
                                // Counted apart from `scopes_failed`: a silence
                                // read that could not run is not a register
                                // that could not be written, and reporting one
                                // as the other sends somebody to the wrong log.
                                Some(Err(error)) => {
                                    snapshot.silence_scopes_failed += 1;
                                    warn!(target: LOG_TARGET, %error, "silence watch failed for one scope");
                                },
                                None => {},
                            }
                        },
                        // A scope that failed is counted and the tick
                        // continues: one tenant's unreadable ledger must not
                        // stop another tenant's complaints from being
                        // recorded. The tick still ends degraded.
                        //
                        // The silence watch never ran for that scope either, so
                        // it is counted as unread rather than as clear — "we
                        // could not look" must never fold into "nothing there".
                        //
                        // The intake runs first inside the pass, so a sweep
                        // failure discards whatever it reported. That is
                        // counted as unread rather than as zero: any receipt it
                        // did record is in the ledger and the cursor did not
                        // move, so the next tick sweeps it — but this tick
                        // cannot say so, and saying nothing is not the same as
                        // saying none.
                        Ok(Err(error)) => {
                            snapshot.scopes_failed += 1;
                            if watch.is_some() {
                                snapshot.silence_scopes_failed += 1;
                            }
                            if receipts.is_some() {
                                snapshot.receipts_scopes_failed += 1;
                            }
                            warn!(target: LOG_TARGET, %error, "delivery-hygiene sweep failed for one scope");
                        },
                        Err(error) => {
                            snapshot.scopes_failed += 1;
                            if watch.is_some() {
                                snapshot.silence_scopes_failed += 1;
                            }
                            if receipts.is_some() {
                                snapshot.receipts_scopes_failed += 1;
                            }
                            warn!(target: LOG_TARGET, %error, "delivery-hygiene sweep task did not complete");
                        },
                    }
                }

                let mut snapshot = worker_health.snapshot.write().await;
                finalize_tick(&mut snapshot, &Utc::now());
                finalize_silence(&mut snapshot);
                finalize_receipts(&mut snapshot);
                // Emitted, not merely stored — the same reason the silence
                // warning is. An act leaving `dispatch_unknown` is the first
                // evidence this subsystem has ever produced that a send is
                // accountable, and it should be legible in the log rather than
                // only to somebody who thought to open the health endpoint.
                if snapshot.acts_left_dispatch_unknown > 0 {
                    info!(
                        target: LOG_TARGET,
                        source = snapshot.receipts_source.as_deref().unwrap_or("unnamed"),
                        offered = snapshot.receipts_offered,
                        recorded = snapshot.receipts_recorded,
                        acts_left_dispatch_unknown = snapshot.acts_left_dispatch_unknown,
                        acts_left_dispatch_unknown_total = snapshot.acts_left_dispatch_unknown_total,
                        disclosures_advanced = snapshot.disclosures_advanced,
                        "provider receipts reconciled acts out of dispatch_unknown"
                    );
                }
                if snapshot.receipts_state == "degraded" {
                    warn!(
                        target: LOG_TARGET,
                        offered = snapshot.receipts_offered,
                        uncorrelated = snapshot.receipts_uncorrelated,
                        unreadable = snapshot.receipts_unreadable,
                        refused = snapshot.receipts_refused,
                        scopes_failed = snapshot.receipts_scopes_failed,
                        detail = snapshot.receipts_last_error.as_deref().unwrap_or(""),
                        "provider receipts arrived that could not be accounted for"
                    );
                }
                if snapshot.suppressed > 0 {
                    info!(
                        target: LOG_TARGET,
                        considered = snapshot.considered,
                        suppressed = snapshot.suppressed,
                        already_held = snapshot.already_held,
                        "delivery-hygiene sweep recorded suppressions"
                    );
                }
                // The early warning, emitted rather than merely stored. A
                // snapshot only helps somebody already looking; a rail that has
                // gone quiet has to reach somebody who is not.
                if snapshot.silence_state == "degraded" {
                    warn!(
                        target: LOG_TARGET,
                        dispatched_scanned = snapshot.dispatched_scanned,
                        acknowledged = snapshot.acknowledged,
                        unacknowledged = snapshot.unacknowledged,
                        overdue = snapshot.overdue,
                        longest_silence_secs = snapshot.longest_silence_secs.unwrap_or(0),
                        detail = snapshot.silence_last_error.as_deref().unwrap_or(""),
                        "acts were dispatched and never acknowledged"
                    );
                }
            }
            info!(target: LOG_TARGET, "delivery-hygiene sweep cancelled");
        });

        Self {
            handle: Some(handle),
            cancel,
            health,
        }
    }

    pub fn health(&self) -> SuppressionSweepHealth {
        self.health.clone()
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle {
            let _ = handle.await;
        }
    }
}

/// One scope's sweep — the blocking half, so it can be run off the reactor.
///
/// The window opens at the cursor and closes at the instant this pass began
/// reading, **not** at the instant it finished. The ledger's boundary is
/// inclusive on the lower end, so a signal recorded exactly at the close is
/// re-offered by the next tick rather than falling between two windows.
///
/// The cursor advances only after the ingest returned. A crash in between
/// leaves the cursor where it was, and the next tick re-reads a window whose
/// rows the register already holds — the idempotent case, which costs one read.
/// The reverse order would silently skip every signal in the window it had
/// already claimed.
/// The silence read runs **after** the register work and its failure is carried
/// rather than raised. The sweep writes and the watch only reads, so an
/// unreadable dispatch log must not be able to stop a complaint from reaching
/// the register — but it must still be reported, which is what the carried
/// `Err` is for.
///
/// # The intake runs FIRST, and its failure is carried too
///
/// Receipts are pulled before `opened_at` is taken, so a hard bounce that
/// arrives on this tick is inside the window this tick sweeps and the cursor
/// that advances past it covers it. Pulling after the sweep would be one line
/// simpler and would make every bounce wait a whole interval before it could
/// suppress anybody — fifteen minutes, by default, during which the address is
/// still sendable.
///
/// Its failure is carried for the same reason the watch's is, in the other
/// direction: an intake nobody can read must not stop the bounces the ledger
/// **already holds** from reaching the register. A source that is down is a
/// reason to record less, never a reason to suppress less.
fn sweep_one(
    workspace_layout: &ArtifactV2Workspace,
    scope: &HygieneScope,
    watch: Option<SilenceWatch>,
    receipts: Option<&dyn ReceiptPuller>,
) -> Result<ScopePass> {
    let ledger = DeliveryLedger::new(workspace_layout.clone());
    let register = SuppressionRegister::global(workspace_layout.clone());
    let cursor = HygieneCursor::new(workspace_layout.clone());

    let delivery_scope = DeliveryScope::new(scope.principal.clone(), scope.workspace.clone());
    let suppression_scope = SuppressionScope::new(scope.principal.clone(), scope.workspace.clone());

    let receipts = receipts.map(|puller| {
        // The door, not the ledger. Every pulled receipt takes the same route a
        // posted one does, so the act-must-have-left check and the attempt log
        // apply identically and there is no second way into the ledger.
        let door = ReceiptIntake::new(workspace_layout.clone());
        let dispatch_log = DispatchLog::new(workspace_layout.clone());
        let disclosures = OutwardAssertionStore::new(workspace_layout.clone());
        pull_and_admit(
            &door,
            &dispatch_log,
            &disclosures,
            &delivery_scope,
            puller,
            Utc::now(),
        )
        .map_err(|error| format!("{error:#}"))
    });

    let opened_at = Utc::now();
    let since = cursor.read_from(&suppression_scope)?;
    let swept = sweep_delivery_into_suppression(
        &ledger,
        &delivery_scope,
        &register,
        &suppression_scope,
        since,
        opened_at,
    )?;
    cursor.advance_to(&suppression_scope, opened_at)?;

    let silence = watch.map(|watch| {
        watch_one(workspace_layout, &ledger, &delivery_scope, watch)
            .map_err(|error| format!("{error:#}"))
    });
    Ok(ScopePass {
        receipts,
        swept,
        silence,
    })
}

/// One scope's silence read.
///
/// A fresh `Utc::now()` rather than the sweep's `opened_at`: every silence is
/// derived from the clock on the read that reports it, and reusing an instant
/// taken before a slow register write would understate every act's age by
/// however long that write took.
fn watch_one(
    workspace_layout: &ArtifactV2Workspace,
    ledger: &DeliveryLedger,
    scope: &DeliveryScope,
    watch: SilenceWatch,
) -> Result<SilenceReport> {
    let dispatch_log = DispatchLog::new(workspace_layout.clone());
    let scan = scan_silence(&dispatch_log, ledger, scope, watch.grace, Utc::now())?;

    // Attribution is resolved only for the acts the report names — the
    // unacknowledged ones. Reading a disclosure for every act ever dispatched
    // would grow with the log forever to answer a question about the acts that
    // are not in it.
    let disclosures = OutwardAssertionStore::new(workspace_layout.clone());
    let outward_scope = OutwardScope::new(scope.principal.clone(), scope.workspace.clone());
    let rails = rails_from_disclosures(&disclosures, &outward_scope, &scan.act_refs())?;

    attribute(&scan, &rails, watch.named_acts)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::DeliveryHygieneScopeConfig;
    use crate::magician_v2::delivery::{DeliveryReceipt, DeliveryState};
    use crate::magician_v2::delivery_receipts::pull::{BounceMailbox, DsnPuller, InboundMail};
    use crate::magician_v2::delivery_receipts::{
        open_local_sent_index, SendIdentifier, SentMessage,
    };
    use crate::magician_v2::evidence::OutwardActStatus;

    fn config() -> DeliveryHygieneConfig {
        DeliveryHygieneConfig::default()
    }

    /// A tick that swept nothing must not report success.
    ///
    /// An unconfigured scope list and a week with no bounces produce identical
    /// counts, and the first is the state this whole worker exists to escape —
    /// the sweep running, every number zero, and not one identity ever
    /// reaching the register. Reading that as `idle` is how a subsystem stays
    /// broken for a year behind a green dashboard.
    #[test]
    fn a_tick_that_swept_no_scopes_is_degraded_not_idle() {
        let now = Utc::now();
        let mut empty = SuppressionSweepHealthSnapshot::configured(&config());
        finalize_tick(&mut empty, &now);
        assert_eq!(empty.state, "degraded");
        assert!(empty.last_error.is_some());
        assert_eq!(
            empty.last_success_at, None,
            "a tick that swept nothing must not stamp a success time"
        );
    }

    /// A quiet week is not a failure: ledgers were read and nobody had bounced.
    #[test]
    fn a_tick_that_swept_scopes_and_suppressed_nobody_is_idle() {
        let now = Utc::now();
        let mut quiet = SuppressionSweepHealthSnapshot::configured(&config());
        quiet.scopes_seen = 2;
        finalize_tick(&mut quiet, &now);
        assert_eq!(quiet.state, "idle");
        assert_eq!(quiet.last_success_at, Some(now.to_rfc3339()));
        assert_eq!(quiet.suppressed, 0);
    }

    /// One failed scope keeps the tick degraded even though others worked, so
    /// a tenant whose ledger is unreadable every tick stays visible rather than
    /// being averaged away.
    #[test]
    fn one_failed_scope_degrades_a_tick_that_otherwise_worked() {
        let now = Utc::now();
        let mut partial = SuppressionSweepHealthSnapshot::configured(&config());
        partial.scopes_seen = 3;
        partial.scopes_failed = 1;
        partial.suppressed = 2;
        finalize_tick(&mut partial, &now);
        assert_eq!(partial.state, "degraded");
        assert_eq!(
            partial.last_success_at, None,
            "a degraded tick must not stamp a success time"
        );
    }

    /// The sweep is on out of the box, and the interval is the conservative one.
    ///
    /// This worker only ever **adds** suppressions a provider already asserted,
    /// so the failure mode of not running it is mailing people who bounced or
    /// complained. That is the opposite of the maturity sweep, which records
    /// judgements against a window somebody has to choose and is therefore off
    /// until configured — and the difference is why this default is not a
    /// copy-paste of that one.
    #[test]
    fn the_sweep_ships_on_with_a_conservative_interval_and_a_real_scope() {
        let default = config();
        assert!(default.enabled);
        assert!(!default.paused);
        assert_eq!(default.tick_interval_secs, 900);
        assert_eq!(
            default.scopes,
            vec![DeliveryHygieneScopeConfig {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            }]
        );

        let snapshot = SuppressionSweepHealthSnapshot::configured(&default);
        assert_eq!(snapshot.state, "idle");
        assert_eq!(snapshot.tick_interval_secs, 900);
    }

    /// An interval below the floor becomes the floor, never a spin.
    #[test]
    fn a_zero_interval_is_raised_to_the_floor() {
        assert_eq!(0_u64.max(MIN_TICK_INTERVAL_SECS), 60);
        assert_eq!(900_u64.max(MIN_TICK_INTERVAL_SECS), 900);
    }

    /// Enabled with no scopes must not start a worker that ticks forever
    /// reporting zeros — it must refuse, visibly.
    #[tokio::test]
    async fn enabled_with_no_scopes_refuses_to_start() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let worker = SuppressionSweepWorker::spawn(
            ArtifactV2Workspace::new(tmp.path()),
            DeliveryHygieneConfig {
                enabled: true,
                scopes: Vec::new(),
                ..config()
            },
            CancellationToken::new(),
        );
        let snapshot = worker.health().snapshot().await;
        assert_eq!(snapshot.state, "degraded");
        assert!(
            snapshot
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("names no scope")),
            "{:?}",
            snapshot.last_error
        );
        worker.shutdown().await;
    }

    /// One pass of the real work: a hard bounce in the ledger becomes an entry
    /// on the register, the cursor moves past it, and a second pass over the
    /// same ledger writes nothing new.
    ///
    /// Pins the whole point of this file. The register is asserted EMPTY first,
    /// then the ledger is asserted to hold the signal, and only then is the
    /// suppression asserted — a test that skipped the first two steps would
    /// pass against a sweep that did nothing at all.
    #[test]
    fn a_pass_writes_the_bounce_and_moves_the_cursor_past_it() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let scope = HygieneScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let suppression_scope = SuppressionScope::new("anonymous", "default");
        let register = SuppressionRegister::global(layout.clone());
        let cursor = HygieneCursor::new(layout.clone());

        // Nobody is suppressed yet — the state the whole programme was stuck in.
        assert_eq!(
            register
                .history(&suppression_scope, "dead@example.test")
                .expect("history reads"),
            Vec::new()
        );
        assert_eq!(
            cursor.swept_through(&suppression_scope).expect("reads"),
            None
        );

        let ledger = DeliveryLedger::new(layout.clone());
        ledger
            .reconcile(
                &DeliveryScope::new("anonymous", "default"),
                "act-1",
                &DeliveryReceipt {
                    provider: "postal".to_string(),
                    provider_message_id: "pm-1".to_string(),
                    identity: "dead@example.test".to_string(),
                    state: DeliveryState::Bounced { hard: true },
                    observed_at: Utc::now(),
                    payload_ref: "hook-1".to_string(),
                },
                Utc::now(),
            )
            .expect("the provider receipt records");

        let first = sweep_one(&layout, &scope, None, None)
            .expect("a pass runs")
            .swept;
        assert_eq!(first.considered, 1);
        assert_eq!(first.suppressed, 1);

        let entries = register
            .history(&suppression_scope, "dead@example.test")
            .expect("history reads");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].reason,
            crate::magician_v2::suppression::SuppressionReason::HardBounce
        );

        let moved = cursor
            .swept_through(&suppression_scope)
            .expect("reads")
            .expect("the cursor advanced past the window it swept");

        // A second pass reads from the moved cursor and finds the ledger's
        // window empty, so it neither considers nor writes anything.
        let second = sweep_one(&layout, &scope, None, None)
            .expect("a second pass runs")
            .swept;
        assert_eq!(second.considered, 0);
        assert_eq!(second.suppressed, 0);
        assert!(
            cursor
                .swept_through(&suppression_scope)
                .expect("reads")
                .expect("still set")
                >= moved,
            "the cursor must never walk backwards"
        );

        // And the register still holds exactly one entry, unmoved.
        let after = register
            .history(&suppression_scope, "dead@example.test")
            .expect("history reads");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].suppressed_at, entries[0].suppressed_at);
    }

    // ── The silence watch ───────────────────────────────────────────────────

    /// The watch is on out of the box, with a grace slower than any honest
    /// in-flight window and a floor of one.
    ///
    /// The floor matters: an act no provider acknowledged a full day after it
    /// left is already the failure this watch exists to surface, and a higher
    /// default would be a decision to stay quiet about the first few — taken by
    /// nobody, in a file nobody would think to read.
    #[test]
    fn the_silence_watch_ships_on_with_a_days_grace_and_a_floor_of_one() {
        let default = config();
        assert!(default.silence_watch_enabled);
        assert_eq!(default.silence_grace_hours, 24);
        assert_eq!(default.silence_alert_overdue, 1);
        assert_eq!(default.silence_named_acts, 20);

        let snapshot = SuppressionSweepHealthSnapshot::configured(&default);
        assert_eq!(snapshot.silence_state, "idle");
        assert_eq!(snapshot.silence_grace_hours, 24);
        assert_eq!(snapshot.silence_alert_overdue, 1);
    }

    /// Overdue acts at or above the floor report `degraded` and NAME the rail.
    ///
    /// This is the early warning itself. Without it a provider integration that
    /// silently stopped answering produces no signal at all — every send
    /// succeeds, no receipt ever arrives, and the only difference from a
    /// working system is a number nobody computes. The rail is named because
    /// "something is quiet" sends nobody anywhere; "email (7)" does.
    #[test]
    fn overdue_acts_at_the_floor_report_degraded_and_name_the_worst_rail() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        snapshot.scopes_seen = 1;
        snapshot.dispatched_scanned = 9;
        snapshot.acknowledged = 2;
        snapshot.unacknowledged = 7;
        snapshot.overdue = 7;
        snapshot.overdue_by_rail = BTreeMap::from([
            ("email".to_string(), 6usize),
            ("whatsapp".to_string(), 1usize),
        ]);
        finalize_silence(&mut snapshot);

        assert_eq!(snapshot.silence_state, "degraded");
        let error = snapshot
            .silence_last_error
            .clone()
            .expect("a stated reason");
        assert!(error.contains("7 of 9 dispatched act(s)"), "{error}");
        assert!(error.contains("email (6)"), "{error}");

        // The register half is untouched: a rail going quiet is not a failure
        // to record suppressions, and folding them would send somebody to the
        // wrong log.
        assert_eq!(snapshot.state, "idle");
    }

    /// Acts dispatched, receipts received, nothing overdue: `idle`.
    ///
    /// Without this the test above would also pass for a watch that reported
    /// `degraded` unconditionally, which is a signal nobody would keep reading.
    #[test]
    fn acknowledged_dispatches_with_nothing_overdue_are_idle() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        snapshot.scopes_seen = 1;
        snapshot.dispatched_scanned = 9;
        snapshot.acknowledged = 9;
        finalize_silence(&mut snapshot);
        assert_eq!(snapshot.silence_state, "idle");
        assert_eq!(snapshot.silence_last_error, None);
    }

    /// A scope that has never dispatched anything is `no_dispatches`, never
    /// `idle`.
    ///
    /// Pins the vacuous pass at the health layer. Zero overdue over zero
    /// dispatches and zero overdue over four hundred acknowledged acts are
    /// opposite facts, and rendering the first as a quiet tick is precisely how
    /// this programme's modules stayed inert behind green dashboards.
    #[test]
    fn a_tick_that_found_no_dispatches_anywhere_is_not_idle() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        snapshot.scopes_seen = 2;
        snapshot.scopes_with_no_dispatches = 2;
        finalize_silence(&mut snapshot);
        assert_eq!(snapshot.silence_state, "no_dispatches");
        assert!(
            snapshot
                .silence_last_error
                .as_deref()
                .is_some_and(|error| error.contains("not evidence that sending works")),
            "{:?}",
            snapshot.silence_last_error
        );
    }

    /// A scope whose silence could not be read degrades the watch and leaves
    /// the register sweep's own state alone.
    #[test]
    fn an_unreadable_scope_degrades_the_watch_by_itself() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        snapshot.scopes_seen = 2;
        snapshot.silence_scopes_failed = 1;
        finalize_silence(&mut snapshot);
        assert_eq!(snapshot.silence_state, "degraded");
        assert!(
            snapshot
                .silence_last_error
                .as_deref()
                .is_some_and(|error| error.contains("never an empty one")),
            "{:?}",
            snapshot.silence_last_error
        );
        assert_eq!(snapshot.state, "idle", "the register sweep is unaffected");
    }

    /// A tick never erases the sentence explaining why a watch was refused,
    /// and never carries a live watch's previous error forward.
    ///
    /// Two opposite mistakes in one place. A `disabled` state with no stated
    /// reason cannot be told apart from a watch somebody switched off on
    /// purpose; and a silence error carried across ticks would report last
    /// tick's outage as this tick's, long after it cleared.
    #[test]
    fn a_refused_watch_keeps_its_reason_and_a_live_one_starts_clean() {
        let now = Utc::now();

        let mut refused = SuppressionSweepHealthSnapshot::configured(&config());
        refused.silence_state = "disabled".to_string();
        refused.silence_last_error = Some("the grace was unusable".to_string());
        refused.begin_tick(&now);
        assert_eq!(
            refused.silence_last_error.as_deref(),
            Some("the grace was unusable")
        );

        let mut live = SuppressionSweepHealthSnapshot::configured(&config());
        live.silence_last_error = Some("last tick's outage".to_string());
        live.overdue = 4;
        live.unacknowledged = 4;
        live.overdue_by_rail = BTreeMap::from([("email".to_string(), 4usize)]);
        live.begin_tick(&now);
        assert_eq!(live.silence_last_error, None);
        assert_eq!(live.overdue, 0);
        assert_eq!(live.unacknowledged, 0);
        assert!(live.overdue_by_rail.is_empty());
    }

    /// A tick never quietly re-enables a watch that was refused at startup.
    #[test]
    fn a_disabled_watch_stays_disabled_through_finalize() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&DeliveryHygieneConfig {
            silence_watch_enabled: false,
            ..config()
        });
        assert_eq!(snapshot.silence_state, "disabled");
        snapshot.scopes_seen = 1;
        snapshot.overdue = 99;
        finalize_silence(&mut snapshot);
        assert_eq!(snapshot.silence_state, "disabled");
    }

    /// An unusable grace declines the watch visibly and leaves the register
    /// sweep running.
    ///
    /// A zero grace would mark every live send overdue the instant it left, and
    /// a signal that is always on is one nobody reads. Substituting a window
    /// nobody chose would be worse: the number this watch reports is the one
    /// somebody acts on.
    #[tokio::test]
    async fn a_grace_of_zero_declines_the_watch_and_says_why() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let worker = SuppressionSweepWorker::spawn(
            ArtifactV2Workspace::new(tmp.path()),
            DeliveryHygieneConfig {
                silence_grace_hours: 0,
                ..config()
            },
            CancellationToken::new(),
        );
        let snapshot = worker.health().snapshot().await;
        assert_eq!(snapshot.silence_state, "disabled");
        assert!(
            snapshot
                .silence_last_error
                .as_deref()
                .is_some_and(|error| error.contains("says nothing")),
            "{:?}",
            snapshot.silence_last_error
        );
        assert_ne!(
            snapshot.state, "disabled",
            "the register sweep must still run when only the watch was refused"
        );
        worker.shutdown().await;
    }

    /// An alert floor of zero declines the watch too.
    ///
    /// A floor of zero reports every tick as degraded whatever the ledger says
    /// — including a tick where every send was acknowledged. That is the same
    /// failure as a zero grace wearing a different setting: a signal nobody can
    /// distinguish from noise stops being read, and this one is the only early
    /// warning a broken rail produces.
    #[tokio::test]
    async fn an_alert_floor_of_zero_declines_the_watch_and_says_why() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let worker = SuppressionSweepWorker::spawn(
            ArtifactV2Workspace::new(tmp.path()),
            DeliveryHygieneConfig {
                silence_alert_overdue: 0,
                ..config()
            },
            CancellationToken::new(),
        );
        let snapshot = worker.health().snapshot().await;
        assert_eq!(snapshot.silence_state, "disabled");
        assert!(
            snapshot
                .silence_last_error
                .as_deref()
                .is_some_and(|error| error.contains("always on")),
            "{:?}",
            snapshot.silence_last_error
        );
        worker.shutdown().await;
    }

    /// One real pass with the watch on: a dispatched act nothing acknowledged
    /// comes back named, attributed, and with its silence measured.
    ///
    /// The end-to-end pin for tier 3. `DeliveryLedger::unreconciled` had no
    /// caller at all before this, so the candidate list accumulated forever and
    /// nothing ever read it — which is exactly the shape of a subsystem that is
    /// correct and unreachable.
    #[test]
    fn a_pass_with_the_watch_on_names_the_act_nobody_acknowledged() {
        use crate::magician_v2::evidence::{OutwardChannel, PrepareOutwardAct};

        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let scope = HygieneScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let delivery_scope = DeliveryScope::new("anonymous", "default");
        let outward_scope = OutwardScope::new("anonymous", "default");
        let dispatched_at = Utc::now() - Duration::hours(5);

        let disclosures = OutwardAssertionStore::new(layout.clone());
        let act = disclosures
            .prepare(
                &outward_scope,
                &PrepareOutwardAct {
                    idempotency_key: "mail-1".to_string(),
                    program_id: None,
                    engagement_id: None,
                    exact_payload_artifact_ref: "artifact-1".to_string(),
                    effective_sender: "owner@example.test".to_string(),
                    intended_audience: vec!["them@example.test".to_string()],
                    channel: OutwardChannel::Email,
                    consequence_class: "routine".to_string(),
                },
                &dispatched_at.to_rfc3339(),
            )
            .expect("the disclosure prepares");
        DispatchLog::new(layout.clone())
            .record(&delivery_scope, &act.outward_act_ref, dispatched_at)
            .expect("the dispatch records");

        let watch = SilenceWatch {
            grace: Duration::hours(1),
            named_acts: 5,
        };
        let pass = sweep_one(&layout, &scope, Some(watch), None).expect("a pass runs");
        let report = pass
            .silence
            .expect("the watch ran")
            .expect("the watch read the logs");

        assert!(report.any_dispatch_recorded);
        assert_eq!(report.scanned, 1);
        assert_eq!(report.acknowledged, 0);
        assert_eq!(report.unacknowledged, 1);
        assert_eq!(report.overdue, 1);
        assert_eq!(report.acts.len(), 1);
        assert_eq!(report.acts[0].act_ref, act.outward_act_ref);
        assert_eq!(report.acts[0].rail, "email");
        assert!(
            report.acts[0].silent_for_secs >= 5 * 3600,
            "silence is derived from the clock: {}",
            report.acts[0].silent_for_secs
        );
        assert_eq!(
            report
                .by_rail
                .iter()
                .map(|rail| rail.rail.as_str())
                .collect::<Vec<_>>(),
            vec!["email"]
        );

        // And the register sweep in the same pass is untouched by any of it.
        assert_eq!(pass.swept.considered, 0);
        assert_eq!(pass.swept.suppressed, 0);
    }
    // ── Tier 4: send → bounce → suppress → refuse ───────────────────────────

    /// The provider namespace this test's sends and bounces share.
    ///
    /// The send index is keyed on it, so a lookup under a different name finds
    /// nothing and refuses a bounce that was perfectly correlatable. It is a
    /// parameter rather than a constant in the reader for exactly that reason.
    const PROVIDER: &str = "smtp";

    /// The `Message-ID` we minted for the original message, which the reporting
    /// MTA quotes back inside the returned headers.
    const ORIGINAL_MESSAGE_ID: &str = "orig-7f31c2@magican.ai";

    /// A conforming RFC 3464 hard bounce, as an MTA emits one.
    ///
    /// Kept whole rather than reduced to the fields the chain needs, so the
    /// reader has to find them where a real one will: inside the
    /// `message/delivery-status` part, past a human-readable preamble that
    /// names the recipient in prose, with the correlating id in a third part.
    fn hard_bounce_dsn(recipient: &str) -> String {
        format!(
            "From: MAILER-DAEMON@mx.example.test\n\
             To: reach.magican@gmail.com\n\
             Subject: Delivery Status Notification (Failure)\n\
             Date: Fri, 21 Aug 2026 11:00:00 +0000\n\
             Message-ID: <dsn-9001@mx.example.test>\n\
             Content-Type: multipart/report; report-type=delivery-status; boundary=\"B1\"\n\
             \n\
             --B1\n\
             Content-Type: text/plain; charset=utf-8\n\
             \n\
             Your message to {recipient} could not be delivered.\n\
             \n\
             --B1\n\
             Content-Type: message/delivery-status\n\
             \n\
             Reporting-MTA: dns; mx.example.test\n\
             Arrival-Date: Fri, 21 Aug 2026 10:30:00 +0000\n\
             \n\
             Final-Recipient: rfc822; {recipient}\n\
             Action: failed\n\
             Status: 5.1.1\n\
             Remote-MTA: dns; mx.recipient.test\n\
             Diagnostic-Code: smtp; 550 5.1.1 <{recipient}>: Recipient address rejected\n\
             Last-Attempt-Date: Fri, 21 Aug 2026 10:45:00 +0000\n\
             \n\
             --B1\n\
             Content-Type: text/rfc822-headers\n\
             \n\
             Message-ID: <{ORIGINAL_MESSAGE_ID}>\n\
             From: reach.magican@gmail.com\n\
             To: {recipient}\n\
             Subject: The original message\n\
             \n\
             --B1--\n"
        )
    }

    /// A directory of raw mail, as a [`BounceMailbox`].
    ///
    /// The shape a real mailbox takes: messages arrive, are offered until
    /// settled, and settling renames rather than deletes so a report stays
    /// readable for whoever later doubts the suppression it produced. A hard
    /// bounce is not operationally liftable, so the evidence has to outlive the
    /// decision.
    #[derive(Debug)]
    struct DirMailbox {
        dir: std::path::PathBuf,
    }

    impl BounceMailbox for DirMailbox {
        fn name(&self) -> &str {
            "maildir"
        }

        fn unread(&self, _scope: &DeliveryScope, _now: DateTime<Utc>) -> Result<Vec<InboundMail>> {
            let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&self.dir)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            paths.sort();
            let mut out = Vec::new();
            for path in paths {
                if path.extension().and_then(|ext| ext.to_str()) != Some("eml") {
                    continue;
                }
                out.push(InboundMail {
                    handle: path
                        .file_name()
                        .expect("a listed file has a name")
                        .to_string_lossy()
                        .to_string(),
                    raw: std::fs::read_to_string(&path)?,
                });
            }
            Ok(out)
        }

        fn settle(&self, _scope: &DeliveryScope, handle: &str, _now: DateTime<Utc>) -> Result<()> {
            // A bare `fs::rename`, and correct as written — which the
            // store-durability guard cannot tell from the outside, so it is
            // said here. That rule counts hand-rolled ATOMIC WRITES: a rename
            // used to publish new contents over a store file, where skipping
            // `sync_all` and the parent-directory sync makes the rename durable
            // while the bytes are not. Nothing is being written here. This
            // moves an existing message aside to mark it settled, exactly as a
            // maildir does, and there are no contents whose durability the
            // rename could outrun. `write_bytes_atomic` cannot express it: it
            // publishes bytes, and the byte we would have to invent is the
            // message we are trying not to lose.
            std::fs::rename(
                self.dir.join(handle),
                self.dir.join(format!("{handle}.settled")),
            )?;
            Ok(())
        }
    }

    /// Seed one live send exactly as the executor records one.
    ///
    /// Disclosure prepared, marked dispatching, parked at `dispatch_unknown`
    /// with the executor's own literal reason, and written to the dispatch log
    /// the silence watch takes its candidates from. Anything less and the tests
    /// below would prove a chain that starts somewhere no real send does.
    fn seed_live_send(
        layout: &ArtifactV2Workspace,
        recipient: &str,
        dispatched_at: DateTime<Utc>,
    ) -> String {
        use crate::magician_v2::evidence::{OutwardChannel, PrepareOutwardAct};

        let delivery_scope = DeliveryScope::new("anonymous", "default");
        let outward_scope = OutwardScope::new("anonymous", "default");
        let store = OutwardAssertionStore::new(layout.clone());
        let at = dispatched_at.to_rfc3339();

        let act = store
            .prepare(
                &outward_scope,
                &PrepareOutwardAct {
                    idempotency_key: format!("send-to-{recipient}"),
                    program_id: None,
                    engagement_id: None,
                    exact_payload_artifact_ref: "artifact-rev-1".to_string(),
                    effective_sender: "reach.magican@gmail.com".to_string(),
                    intended_audience: vec![recipient.to_string()],
                    channel: OutwardChannel::Email,
                    consequence_class: "routine".to_string(),
                },
                &at,
            )
            .expect("the disclosure prepares");
        store
            .mark_dispatching(&outward_scope, &act.outward_act_ref, &at)
            .expect("the act is in flight");
        store
            .mark_dispatch_unknown(
                &outward_scope,
                &act.outward_act_ref,
                "no adapter receipt support: this send cannot be reconciled",
                &at,
            )
            .expect("the act parks at dispatch_unknown");
        DispatchLog::new(layout.clone())
            .record(&delivery_scope, &act.outward_act_ref, dispatched_at)
            .expect("the dispatch records");
        act.outward_act_ref
    }

    /// Record which message a send became, so a bounce can be tied back to it.
    fn capture_message_id(
        layout: &ArtifactV2Workspace,
        act_ref: &str,
        recipient: &str,
        sent_at: DateTime<Utc>,
    ) {
        open_local_sent_index(layout.clone())
            .record(
                &DeliveryScope::new("anonymous", "default"),
                PROVIDER,
                &SendIdentifier::MessageId(ORIGINAL_MESSAGE_ID.to_string()),
                &SentMessage {
                    act_ref: act_ref.to_string(),
                    audience: vec![recipient.to_string()],
                    sent_at,
                },
                sent_at,
            )
            .expect("send-time capture records");
    }

    fn status_of(layout: &ArtifactV2Workspace, act_ref: &str) -> OutwardActStatus {
        OutwardAssertionStore::new(layout.clone())
            .load_act(&OutwardScope::new("anonymous", "default"), act_ref)
            .expect("the disclosure reads")
            .expect("the disclosure exists")
            .status
    }

    fn puller(layout: &ArtifactV2Workspace, inbox: &std::path::Path) -> DsnPuller {
        DsnPuller::new(
            PROVIDER,
            Arc::new(DirMailbox {
                dir: inbox.to_path_buf(),
            }),
            layout.clone(),
        )
    }

    /// **The whole of tiers 3 and 4 in one run: send, bounce, suppress,
    /// refuse.**
    ///
    /// Pins the failure no single unit could catch, because every link was
    /// correct on its own and the chain did not exist. `DeliveryLedger::reconcile`
    /// had no production caller, so the ledger was empty; the hygiene sweep
    /// carried what the ledger held, which was nothing; the suppression register
    /// stayed empty; and `contact_refusal` — a genuinely fail-closed guard —
    /// screened every recipient against an empty store and cleared all of them.
    /// It would have gone on clearing an address that had hard-bounced, forever.
    ///
    /// The first pass establishes that the address is **sendable**, the act is
    /// **overdue**, the register is **empty** and the disclosure reads
    /// **`dispatch_unknown`** — so nothing below can pass against a store that
    /// was already in the state it is meant to prove was reached.
    #[test]
    fn a_hard_bounce_dsn_suppresses_the_address_and_the_next_send_is_refused() {
        use crate::magician_v2::agents::outward_gate::{contact_refusal, Addressing};
        use crate::magician_v2::delivery::{DeliveryKnowledge, SuppressionCause};
        use crate::magician_v2::suppression::SuppressionReason;

        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let inbox = tmp.path().join("bounce-mailbox");
        std::fs::create_dir_all(&inbox).expect("the mailbox directory");

        let hygiene_scope = HygieneScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let delivery_scope = DeliveryScope::new("anonymous", "default");
        let suppression_scope = SuppressionScope::new("anonymous", "default");
        let register = SuppressionRegister::global(layout.clone());
        let ledger = DeliveryLedger::new(layout.clone());
        let recipient = "Dead@Example.Test";

        // ── The send ────────────────────────────────────────────────────────
        let dispatched_at = Utc::now() - Duration::hours(5);
        let act_ref = seed_live_send(&layout, recipient, dispatched_at);
        capture_message_id(&layout, &act_ref, recipient, dispatched_at);

        let source = puller(&layout, &inbox);
        let watch = SilenceWatch {
            grace: Duration::hours(1),
            named_acts: 5,
        };

        // ── Before: nothing has come back, and the address is sendable ──────
        let before = sweep_one(
            &layout,
            &hygiene_scope,
            Some(watch),
            Some(&source as &dyn ReceiptPuller),
        )
        .expect("a pass over an empty mailbox runs");
        let before_receipts = before
            .receipts
            .expect("the pull ran")
            .expect("an empty mailbox is not a failure");
        assert_eq!(before_receipts.examined, 0, "no bounce has arrived yet");
        assert_eq!(before_receipts.offered, 0);
        assert_eq!(before_receipts.acts_left_dispatch_unknown, 0);
        assert_eq!(
            ledger
                .state_of(&delivery_scope, &act_ref)
                .expect("the ledger reads"),
            DeliveryKnowledge::DispatchUnknown,
            "the act starts exactly where the executor parks every live send"
        );
        assert_eq!(
            status_of(&layout, &act_ref),
            OutwardActStatus::DispatchUnknown
        );
        assert_eq!(
            register
                .history(&suppression_scope, "dead@example.test")
                .expect("the register reads"),
            Vec::new(),
            "the register must be empty here, or every assertion below is vacuous"
        );
        assert_eq!(
            contact_refusal(
                Some(&layout),
                Some("anonymous"),
                Some("default"),
                &[recipient.to_string()],
                Addressing::RecipientRequired,
                Utc::now(),
            ),
            None,
            "this address is sendable before the bounce; without this the refusal below \
             proves nothing"
        );
        let before_silence = before
            .silence
            .expect("the watch ran")
            .expect("the watch read the logs");
        assert_eq!(before_silence.unacknowledged, 1);
        assert_eq!(
            before_silence.overdue, 1,
            "five hours of silence against one hour of grace"
        );

        // ── The bounce arrives as mail ──────────────────────────────────────
        std::fs::write(inbox.join("bounce-1.eml"), hard_bounce_dsn(recipient))
            .expect("the report lands in the mailbox");

        // ── One tick ────────────────────────────────────────────────────────
        let pass = sweep_one(
            &layout,
            &hygiene_scope,
            Some(watch),
            Some(&source as &dyn ReceiptPuller),
        )
        .expect("the tick runs");
        let receipts = pass
            .receipts
            .expect("the pull ran")
            .expect("the mailbox was read");

        assert_eq!(receipts.source, "dsn");
        assert_eq!(receipts.examined, 1);
        assert_eq!(receipts.offered, 1);
        assert_eq!(
            receipts.uncorrelated, 0,
            "send-time capture recorded the message id, so the report ties back to the act"
        );
        assert_eq!(receipts.unreadable, 0);
        assert_eq!(
            receipts.opened, 1,
            "first thing any provider has said about this act"
        );
        assert_eq!(
            receipts.refused, 0,
            "the act is on this scope's dispatch log"
        );
        assert_eq!(receipts.settled, 1);
        assert_eq!(receipts.settle_failures, 0);
        assert_eq!(receipts.disclosure_failures, 0);
        assert_eq!(
            receipts.acts_left_dispatch_unknown, 1,
            "this is the number `/delivery/watch` reports: an act stopped being a question"
        );
        assert_eq!(receipts.disclosures_advanced, 1);
        assert_eq!(receipts.disclosures_left_dispatch_unknown, 1);

        // 1. The act's delivery state advanced, in both records that hold one.
        assert_eq!(
            ledger
                .state_of(&delivery_scope, &act_ref)
                .expect("the ledger reads"),
            DeliveryKnowledge::Observed(DeliveryState::Bounced { hard: true }),
            "5.1.1 is a permanent failure; reading it as transient is the bug that suppresses \
             nobody, and reading a 4.x.x as permanent is the one that suppresses the wrong person"
        );
        assert_eq!(
            status_of(&layout, &act_ref),
            OutwardActStatus::Failed,
            "every recipient's receipt is terminal and none is an arrival, so the disclosure \
             says it reached nobody — and stops owing a correction it could never deliver"
        );

        // 2. A suppression signal was emitted, with the identity normalised.
        assert_eq!(
            ledger
                .suppression_signals(&delivery_scope, DateTime::<Utc>::MIN_UTC)
                .expect("signals read"),
            vec![(
                "dead@example.test".to_string(),
                SuppressionCause::HardBounce
            )]
        );

        // 3. The identity landed on the register — through the one existing
        //    route, on the same tick, because the pull runs before the sweep.
        assert_eq!(pass.swept.considered, 1);
        assert_eq!(pass.swept.suppressed, 1);
        let entries = register
            .history(&suppression_scope, "dead@example.test")
            .expect("the register reads");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reason, SuppressionReason::HardBounce);

        // 4. The next send to that address is REFUSED.
        let refusal = contact_refusal(
            Some(&layout),
            Some("anonymous"),
            Some("default"),
            &[recipient.to_string()],
            Addressing::RecipientRequired,
            Utc::now(),
        )
        .expect("a hard-bounced address must not be contacted again");
        assert!(
            refusal.contains("dead@example.test") && refusal.contains("hard_bounce"),
            "the refusal must name who and why: {refusal}"
        );

        // And the act has left the silence: something finally came back.
        let silence = pass
            .silence
            .expect("the watch ran")
            .expect("the watch read the logs");
        assert_eq!(silence.scanned, 1);
        assert_eq!(silence.acknowledged, 1);
        assert_eq!(silence.unacknowledged, 0);
        assert_eq!(silence.overdue, 0);

        // ── A second tick changes nothing ───────────────────────────────────
        let again = sweep_one(
            &layout,
            &hygiene_scope,
            Some(watch),
            Some(&source as &dyn ReceiptPuller),
        )
        .expect("a second tick runs");
        let again_receipts = again
            .receipts
            .expect("the pull ran")
            .expect("the mailbox was read");
        assert_eq!(
            again_receipts.examined, 0,
            "a settled report is never offered twice"
        );
        assert_eq!(again.swept.suppressed, 0);
        assert_eq!(
            register
                .history(&suppression_scope, "dead@example.test")
                .expect("the register reads")
                .len(),
            1,
            "a second tick must not stack a second entry on one bounce"
        );
        assert!(
            inbox.join("bounce-1.eml.settled").exists(),
            "the report stays readable for whoever doubts the suppression it produced"
        );
    }

    /// A bounce nothing can tie to a send suppresses NOBODY, and is held.
    ///
    /// Pins the fail-closed direction of the same chain. Send-time capture is
    /// the newest link and the one most likely to be missing, and the tempting
    /// repair — attach the bounce to the most recent act to that address — is
    /// how a working mailbox gets suppressed by somebody else's bounce, forever,
    /// because `SuppressionReason::HardBounce` is not operationally liftable.
    /// So an uncorrelated report records nothing, is not settled, and the tick
    /// says `degraded` rather than reporting a clean zero.
    #[test]
    fn an_uncorrelated_bounce_suppresses_nobody_and_is_not_dropped() {
        use crate::magician_v2::agents::outward_gate::{contact_refusal, Addressing};
        use crate::magician_v2::delivery::DeliveryKnowledge;

        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let inbox = tmp.path().join("bounce-mailbox");
        std::fs::create_dir_all(&inbox).expect("the mailbox directory");

        let hygiene_scope = HygieneScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let recipient = "Dead@Example.Test";

        // The send happened and was dispatched; nothing recorded which message
        // it became.
        let act_ref = seed_live_send(&layout, recipient, Utc::now() - Duration::hours(5));
        std::fs::write(inbox.join("bounce-1.eml"), hard_bounce_dsn(recipient))
            .expect("the report lands");

        let source = puller(&layout, &inbox);
        let pass = sweep_one(
            &layout,
            &hygiene_scope,
            None,
            Some(&source as &dyn ReceiptPuller),
        )
        .expect("the tick runs");
        let receipts = pass
            .receipts
            .expect("the pull ran")
            .expect("the mailbox was read");

        assert_eq!(receipts.examined, 1);
        assert_eq!(receipts.uncorrelated, 1);
        assert_eq!(receipts.offered, 0);
        assert_eq!(receipts.recorded(), 0, "nothing was written on a guess");
        assert_eq!(receipts.acts_left_dispatch_unknown, 0);
        assert_eq!(receipts.settled, 0);
        assert!(
            receipts.has_fault(),
            "an unplaceable bounce is never a quiet tick"
        );
        assert!(
            inbox.join("bounce-1.eml").exists() && !inbox.join("bounce-1.eml.settled").exists(),
            "an uncorrelated report stays in the mailbox for the pass — or the person — that \
             can place it"
        );

        assert_eq!(
            DeliveryLedger::new(layout.clone())
                .state_of(&DeliveryScope::new("anonymous", "default"), &act_ref)
                .expect("the ledger reads"),
            DeliveryKnowledge::DispatchUnknown,
            "an unplaceable bounce settles no act's question"
        );
        assert_eq!(
            status_of(&layout, &act_ref),
            OutwardActStatus::DispatchUnknown
        );
        assert_eq!(pass.swept.suppressed, 0);
        assert_eq!(
            contact_refusal(
                Some(&layout),
                Some("anonymous"),
                Some("default"),
                &[recipient.to_string()],
                Addressing::RecipientRequired,
                Utc::now(),
            ),
            None,
            "nobody was suppressed on a bounce nobody could place"
        );

        // And the tick says so rather than reporting a clean zero.
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        snapshot.receipts_source = Some("dsn".to_string());
        // Production attachment clears the initial no-source diagnosis through
        // `attach_receipt_source`; this hand-built snapshot must model the same
        // transition before absorbing the source's actual fault.
        snapshot.receipts_last_error = None;
        snapshot.scopes_seen = 1;
        snapshot.absorb_receipts(&receipts);
        finalize_receipts(&mut snapshot);
        assert_eq!(snapshot.receipts_state, "degraded");
        assert!(
            snapshot
                .receipts_last_error
                .as_deref()
                .is_some_and(|error| error.contains("could not be tied to any act")),
            "{:?}",
            snapshot.receipts_last_error
        );
    }

    /// Ordinary mail in a bounce mailbox is settled and never recorded.
    ///
    /// Recognition is structural, so a human writing *about* a failed delivery
    /// — the exact message whose subject line a lexical matcher would take for
    /// a bounce — produces no receipt at all. Pins that the safe answer is also
    /// the one that does not let the mailbox fill up forever.
    #[test]
    fn a_human_message_about_a_bounce_is_settled_and_records_nothing() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let inbox = tmp.path().join("bounce-mailbox");
        std::fs::create_dir_all(&inbox).expect("the mailbox directory");

        let hygiene_scope = HygieneScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        seed_live_send(
            &layout,
            "Dead@Example.Test",
            Utc::now() - Duration::hours(5),
        );
        std::fs::write(
            inbox.join("human-1.eml"),
            "From: colleague@example.test\n\
             To: reach.magican@gmail.com\n\
             Subject: Undeliverable: your message to Dead@Example.Test bounced\n\
             Date: Fri, 21 Aug 2026 11:30:00 +0000\n\
             Content-Type: text/plain; charset=utf-8\n\
             \n\
             Heads up, this address is dead. Status: 5.1.1, Action: failed.\n",
        )
        .expect("the message lands");

        let source = puller(&layout, &inbox);
        let receipts = sweep_one(
            &layout,
            &hygiene_scope,
            None,
            Some(&source as &dyn ReceiptPuller),
        )
        .expect("the tick runs")
        .receipts
        .expect("the pull ran")
        .expect("the mailbox was read");

        assert_eq!(receipts.examined, 1);
        assert_eq!(
            receipts.offered, 0,
            "a subject line is never evidence of a bounce, and neither is prose quoting a status \
             code"
        );
        assert_eq!(
            receipts.uncorrelated, 0,
            "it was not a report at all, so nothing was refused"
        );
        assert_eq!(receipts.unreadable, 0);
        assert!(!receipts.has_fault());
        assert!(
            inbox.join("human-1.eml.settled").exists(),
            "read and determined not to be a report: this bridge is done with it"
        );
    }

    /// No source attached is `no_source`, never `idle`.
    ///
    /// The state this build actually ships in, and the one that must never
    /// render as a quiet week: with nothing pulling receipts, every live send
    /// stays at `dispatch_unknown` for good, the sweep carries an empty ledger,
    /// and the register `contact_refusal` screens against stays empty.
    #[test]
    fn no_receipt_source_is_reported_as_no_source_with_a_reason() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        assert_eq!(
            snapshot.receipts_state, "no_source",
            "the first snapshot must already say the loop is open"
        );
        snapshot.scopes_seen = 4;
        finalize_receipts(&mut snapshot);
        assert_eq!(snapshot.receipts_state, "no_source");
        assert_eq!(snapshot.acts_left_dispatch_unknown_total, 0);
        assert!(
            snapshot
                .receipts_last_error
                .as_deref()
                .is_some_and(|error| error.contains("dispatch_unknown")),
            "{:?}",
            snapshot.receipts_last_error
        );
    }

    /// A quiet tick with a source attached is `idle`, and the running total is
    /// what says whether anything has ever been reconciled.
    #[test]
    fn a_quiet_tick_with_a_source_is_idle_and_the_total_carries() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        snapshot.receipts_source = Some("dsn".to_string());
        snapshot.scopes_seen = 2;
        snapshot.absorb_receipts(&ReceiptPass {
            source: "dsn".to_string(),
            examined: 2,
            offered: 2,
            opened: 2,
            settled: 2,
            acts_left_dispatch_unknown: 2,
            ..ReceiptPass::default()
        });
        finalize_receipts(&mut snapshot);
        assert_eq!(snapshot.receipts_state, "idle");
        assert_eq!(snapshot.receipts_recorded, 2);
        assert_eq!(snapshot.acts_left_dispatch_unknown, 2);
        assert_eq!(snapshot.acts_left_dispatch_unknown_total, 2);

        // A second tick that found nothing keeps the total and zeroes the tick.
        snapshot.begin_tick(&Utc::now());
        snapshot.scopes_seen = 2;
        snapshot.absorb_receipts(&ReceiptPass {
            source: "dsn".to_string(),
            ..ReceiptPass::default()
        });
        finalize_receipts(&mut snapshot);
        assert_eq!(snapshot.receipts_state, "idle");
        assert_eq!(snapshot.acts_left_dispatch_unknown, 0);
        assert_eq!(
            snapshot.acts_left_dispatch_unknown_total, 2,
            "the running total is the only field that answers `has this ever done anything`"
        );
    }

    /// A scope whose source could not be asked is degraded, and never folds
    /// into the register's own failure count.
    #[test]
    fn an_unreadable_source_is_degraded_and_counted_apart() {
        let mut snapshot = SuppressionSweepHealthSnapshot::configured(&config());
        snapshot.receipts_source = Some("dsn".to_string());
        snapshot.scopes_seen = 3;
        snapshot.receipts_scopes_failed = 1;
        finalize_receipts(&mut snapshot);
        assert_eq!(snapshot.receipts_state, "degraded");
        assert_eq!(
            snapshot.scopes_failed, 0,
            "an unreadable mailbox is not an unwritable register"
        );
        assert!(snapshot.receipts_last_error.is_some());
    }
}
