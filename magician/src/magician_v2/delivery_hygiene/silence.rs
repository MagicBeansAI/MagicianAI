//! What we sent that nothing ever came back for.
//!
//! # The failure this makes visible
//!
//! [`DispatchLog::record`](crate::magician_v2::agents::outward_gate::DispatchLog::record)
//! runs on every live send and
//! [`DeliveryLedger::unreconciled`](crate::magician_v2::delivery::DeliveryLedger::unreconciled)
//! exists to answer *"what did we send that nothing ever came back for"*. Until
//! this module, **nothing called it**. So the failure mode of a silently broken
//! provider integration — every send succeeding, every receipt absent — produced
//! no signal at all, and an act sat at `dispatch_unknown` forever with nothing
//! able to say so. A rising unacknowledged count is the only early warning that
//! a rail has gone quiet, and until something reports it the answer to *"is
//! delivery working"* is a shrug that renders as a green dashboard.
//!
//! # This module records nothing
//!
//! Every number below is derived from the clock and the two logs on the read
//! that asks for it. Nothing is written, nothing is stamped, and no decision is
//! stored — so running it twice records nothing new, trivially, and the sweep
//! bug this codebase has already met once (a ripeness derived *before* checking
//! what was already recorded, so widening a window moved a settled act) has no
//! surface here at all. Widening the grace moves the `overdue` count and moves
//! nothing else, because there is no "else".
//!
//! # Two halves, so a second rail needs nothing here edited
//!
//! [`scan_silence`] reads the dispatch log and the ledger and answers in act
//! refs — it knows nothing about what was sent, on what, or why. [`attribute`]
//! is pure: it takes a **supplied** `act_ref → rail` map and folds the counts.
//! The one adapter that resolves that map from outward disclosures,
//! [`rails_from_disclosures`], is *a* source and not *the* source; a rail whose
//! acts are recorded somewhere else supplies its own map and this file is not
//! touched. That is the same "supplied, never discovered" rule
//! [`DispatchedAct`](crate::magician_v2::delivery::DispatchedAct) states for the
//! candidate list, for the same reason: a second copy of "which rail" kept here
//! would drift from the record that owns it.
//!
//! # Counts, never rates
//!
//! [`SilenceReport`] reports whole numbers and carries `scanned` beside them.
//! There is deliberately no `acknowledged_rate`: "97% acknowledged" over thirty
//! acts and over thirty thousand are different facts, and the first is what a
//! broken integration looks like in its first hour.
//!
//! # Fail closed
//!
//! - A scope with **no recorded dispatches** answers `any_dispatch_recorded:
//!   false` rather than a report of zeroes. "Nothing is unacknowledged" over no
//!   candidates is the vacuous clean bill of health the ledger refuses outright,
//!   and this module refuses to launder it into a quiet-looking report.
//! - An act whose rail could not be named is counted under
//!   [`UNATTRIBUTED`] — never folded into a named rail, and never dropped. A
//!   rail that goes quiet must not be able to hide inside another rail's bucket,
//!   and an act nobody can attribute must not vanish from the total.
//! - An act dispatched **ahead of the clock** is counted apart from both
//!   `acknowledged` and `unacknowledged`. Its silence cannot be measured against
//!   an instant it precedes, and quietly calling it acknowledged is how clock
//!   skew erases a send.
//! - An unreadable dispatch log or ledger propagates. A report that answered
//!   "nothing is silent" because it could not read is worse than no report.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::magician_v2::agents::outward_gate::DispatchLog;
use crate::magician_v2::delivery::{DeliveryLedger, DeliveryScope, UnreconciledAct};
use crate::magician_v2::evidence::{OutwardAssertionStore, OutwardScope};

/// Field separator for derived ids across this codebase. A scope that carries
/// one could shear a path or an id apart, so it is refused here as everywhere
/// else — **before** any read, because a crafted scope whose dispatch log
/// happens to be absent would otherwise reach the "nothing dispatched" answer
/// without ever being checked.
const FIELD_SEP: char = '\u{1f}';

/// The bucket an act whose rail could not be named is counted in.
///
/// A reserved word: [`attribute`] refuses a supplied rail spelled this way, so
/// a real rail can never land in — or be mistaken for — the bucket that means
/// *"we do not know"*.
pub const UNATTRIBUTED: &str = "unattributed";

/// Which outward rail an act left on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rail {
    /// Named by whatever record owns the act.
    Named(String),
    /// Nothing could name one. Its own bucket, always.
    Unattributed,
}

impl Rail {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Named(name) => name.as_str(),
            Self::Unattributed => UNATTRIBUTED,
        }
    }
}

/// One act dispatched and never acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SilentAct {
    pub act_ref: String,
    /// The rail's name, or [`UNATTRIBUTED`].
    pub rail: String,
    pub dispatched_at: DateTime<Utc>,
    /// Derived from the clock on this read, never stored. Nothing has to have
    /// run for an act to be overdue.
    pub silent_for_secs: i64,
}

/// One rail's share of the silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RailSilence {
    pub rail: String,
    /// Acts on this rail that nothing has ever acknowledged, at any age.
    pub unacknowledged: usize,
    /// Of those, the ones silent for at least the grace period.
    pub overdue: usize,
    /// The longest silence on this rail, in seconds.
    pub longest_silence_secs: i64,
}

/// What the dispatch log and the ledger say, before anything is attributed.
///
/// Act refs only. Nothing here knows what a rail is, which is what lets one
/// scan serve an owner report, a worker's health snapshot, and a rail that has
/// not been built yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilenceScan {
    pub grace: Duration,
    /// Whether this scope has ever recorded a dispatch. `false` is **not** a
    /// clean bill of health — see the header.
    pub any_dispatch_recorded: bool,
    /// Distinct acts this scope has recorded as dispatched.
    pub scanned: usize,
    /// Acts whose recorded dispatch is later than `now`. Counted apart from
    /// both acknowledged and unacknowledged, because a silence cannot be
    /// measured against an instant it precedes.
    pub dispatched_ahead_of_the_clock: usize,
    /// Every act with no receipt at all, at any age, oldest first.
    pub unacknowledged: Vec<UnreconciledAct>,
    /// The subset silent for at least `grace`, oldest first.
    pub overdue: Vec<UnreconciledAct>,
}

impl SilenceScan {
    /// A scope that has never dispatched anything.
    ///
    /// Named rather than derived so the caller cannot reach it by accident: the
    /// ledger refuses a sweep over no candidates precisely so an empty answer
    /// cannot read as a healthy one, and this is the explicit, flagged form of
    /// that same fact rather than a way around it.
    pub fn nothing_dispatched(grace: Duration) -> Self {
        Self {
            grace,
            any_dispatch_recorded: false,
            scanned: 0,
            dispatched_ahead_of_the_clock: 0,
            unacknowledged: Vec::new(),
            overdue: Vec::new(),
        }
    }

    /// The acts a caller needs to attribute — the unacknowledged ones, which
    /// are the only acts the report names.
    pub fn act_refs(&self) -> Vec<String> {
        self.unacknowledged
            .iter()
            .map(|act| act.act_ref.clone())
            .collect()
    }
}

/// Whole numbers over one scope's dispatch log. No rates — see the header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SilenceReport {
    pub any_dispatch_recorded: bool,
    pub grace_secs: i64,
    /// Distinct acts recorded as dispatched. The denominator, carried, so six
    /// silences over eight acts cannot render like six over eight thousand.
    pub scanned: usize,
    /// `scanned` less the unacknowledged and less those dispatched ahead of the
    /// clock. Arithmetic over three independently established counts, and the
    /// number that answers *"has a receipt ever arrived in this scope"*.
    pub acknowledged: usize,
    pub unacknowledged: usize,
    pub overdue: usize,
    pub dispatched_ahead_of_the_clock: usize,
    /// The longest silence anywhere in this scope, in seconds. `None` only when
    /// nothing is unacknowledged.
    pub longest_silence_secs: Option<i64>,
    /// Counts per rail, worst first. Includes the [`UNATTRIBUTED`] bucket
    /// whenever anything landed in it.
    pub by_rail: Vec<RailSilence>,
    /// The overdue acts, **oldest first**, capped by the caller's limit.
    pub acts: Vec<SilentAct>,
    /// Overdue acts beyond the cap. Reported rather than silently dropped: a
    /// truncated list that looked complete would understate the outage.
    pub acts_omitted: usize,
}

impl SilenceReport {
    /// Whether this scope has anything an operator must look at.
    ///
    /// `any_dispatch_recorded` is load-bearing rather than defensive noise: a
    /// scope with no dispatches has an `overdue` of zero, and reading that as
    /// *"delivery is fine"* is the vacuous pass the whole module refuses.
    pub fn is_quiet(&self) -> bool {
        self.any_dispatch_recorded && self.overdue == 0
    }
}

/// Read the dispatch log and the ledger for one scope.
///
/// # Why the ledger is asked twice
///
/// Once with a zero grace for everything unacknowledged, once with the real
/// grace for the overdue subset. Filtering the first list locally would be one
/// walk cheaper and would put a second copy of the ledger's *inclusive*
/// boundary rule in this file — and a boundary rule that exists in two places
/// is a boundary rule that will eventually disagree with itself. The ledger
/// owns `silent_for >= older_than`; this module never restates it.
pub fn scan_silence(
    dispatch_log: &DispatchLog,
    ledger: &DeliveryLedger,
    scope: &DeliveryScope,
    grace: Duration,
    now: DateTime<Utc>,
) -> Result<SilenceScan> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator that \
             keeps derived ids' components apart, and a crafted scope could otherwise read \
             another owner's dispatch log"
        );
    }
    if grace < Duration::zero() {
        anyhow::bail!(
            "a negative grace period would mark acts overdue before they were dispatched"
        );
    }

    let dispatched = dispatch_log
        .dispatched(scope)
        .context("reading the dispatch log")?;
    if dispatched.is_empty() {
        return Ok(SilenceScan::nothing_dispatched(grace));
    }

    let scanned = dispatched.len();
    let dispatched_ahead_of_the_clock = dispatched
        .iter()
        .filter(|act| act.dispatched_at > now)
        .count();

    let unacknowledged = dispatch_log
        .unreconciled(ledger, scope, Duration::zero(), now)
        .context("asking the ledger what has never been acknowledged")?;
    let overdue = dispatch_log
        .unreconciled(ledger, scope, grace, now)
        .context("asking the ledger what has been silent past the grace period")?;

    Ok(SilenceScan {
        grace,
        any_dispatch_recorded: true,
        scanned,
        dispatched_ahead_of_the_clock,
        unacknowledged,
        overdue,
    })
}

/// Fold a scan into counts, attributing each act to a rail.
///
/// Pure. `rails` is supplied by the caller from whatever record owns the acts;
/// an act absent from it is [`UNATTRIBUTED`], which is a bucket rather than a
/// silence.
///
/// `limit` caps the named act list and is refused at zero: a report that names
/// nothing is indistinguishable from a scope with nothing to name.
pub fn attribute(
    scan: &SilenceScan,
    rails: &BTreeMap<String, String>,
    limit: usize,
) -> Result<SilenceReport> {
    if limit == 0 {
        anyhow::bail!(
            "a silence report must be allowed to name at least one act: a report that names \
             nothing reads exactly like a scope with nothing to report"
        );
    }
    for (act_ref, rail) in rails {
        let trimmed = rail.trim();
        if trimmed.is_empty() {
            anyhow::bail!(
                "act `{act_ref}` was attributed to a blank rail; a blank name would render as \
                 its own bucket and hide which integration went quiet"
            );
        }
        if trimmed == UNATTRIBUTED {
            anyhow::bail!(
                "act `{act_ref}` was attributed to `{UNATTRIBUTED}`, which is the reserved \
                 bucket for an act nothing could name; a real rail spelled this way would be \
                 indistinguishable from one nobody could identify"
            );
        }
    }

    let overdue_refs: BTreeSet<&str> = scan
        .overdue
        .iter()
        .map(|act| act.act_ref.as_str())
        .collect();

    let rail_of = |act_ref: &str| match rails.get(act_ref) {
        Some(rail) => Rail::Named(rail.trim().to_string()),
        None => Rail::Unattributed,
    };

    let mut buckets: BTreeMap<String, RailSilence> = BTreeMap::new();
    let mut longest_silence_secs: Option<i64> = None;
    for act in &scan.unacknowledged {
        let rail = rail_of(&act.act_ref);
        let silent_for = act.silent_for.num_seconds();
        longest_silence_secs =
            Some(longest_silence_secs.map_or(silent_for, |held: i64| held.max(silent_for)));
        let bucket = buckets
            .entry(rail.as_str().to_string())
            .or_insert_with(|| RailSilence {
                rail: rail.as_str().to_string(),
                unacknowledged: 0,
                overdue: 0,
                longest_silence_secs: 0,
            });
        bucket.unacknowledged += 1;
        bucket.longest_silence_secs = bucket.longest_silence_secs.max(silent_for);
        if overdue_refs.contains(act.act_ref.as_str()) {
            bucket.overdue += 1;
        }
    }

    let mut by_rail: Vec<RailSilence> = buckets.into_values().collect();
    // Worst first, so the rail that has gone quiet is the first line an
    // operator reads. Ties break on the name, so two ticks with identical
    // counts render identically.
    by_rail.sort_by(|left, right| {
        right
            .overdue
            .cmp(&left.overdue)
            .then_with(|| right.unacknowledged.cmp(&left.unacknowledged))
            .then_with(|| left.rail.cmp(&right.rail))
    });

    let acts: Vec<SilentAct> = scan
        .overdue
        .iter()
        .take(limit)
        .map(|act| SilentAct {
            act_ref: act.act_ref.clone(),
            rail: rail_of(&act.act_ref).as_str().to_string(),
            dispatched_at: act.dispatched_at,
            silent_for_secs: act.silent_for.num_seconds(),
        })
        .collect();

    Ok(SilenceReport {
        any_dispatch_recorded: scan.any_dispatch_recorded,
        grace_secs: scan.grace.num_seconds(),
        scanned: scan.scanned,
        // Saturating on purpose. An act dispatched ahead of the clock is in
        // `dispatched_ahead_of_the_clock` and may also be reconciled, so the
        // three counts can overlap by exactly that set; a subtraction that
        // wrapped would report an enormous acknowledged count from a one-second
        // clock skew.
        acknowledged: scan
            .scanned
            .saturating_sub(scan.unacknowledged.len())
            .saturating_sub(scan.dispatched_ahead_of_the_clock),
        unacknowledged: scan.unacknowledged.len(),
        overdue: scan.overdue.len(),
        dispatched_ahead_of_the_clock: scan.dispatched_ahead_of_the_clock,
        longest_silence_secs,
        by_rail,
        acts_omitted: scan.overdue.len().saturating_sub(limit),
        acts,
    })
}

/// Resolve `act_ref → rail` from the outward disclosure each act already has.
///
/// **One adapter, not the contract.** [`attribute`] takes the map, so this is
/// the source for acts recorded as outward disclosures and nothing more; a rail
/// that records its acts elsewhere builds its own map and nothing in this file
/// changes. The rail is the disclosure's own `channel`, which is the record
/// that owns the fact — copying it onto the dispatch row would be a second copy
/// that drifts.
///
/// An act with no disclosure is **omitted** rather than defaulted, so it lands
/// in the [`UNATTRIBUTED`] bucket where an operator can see it. An unreadable
/// store propagates: attributing every act to "unknown" because the store was
/// down would report a rail-wide outage as a naming problem.
pub fn rails_from_disclosures(
    store: &OutwardAssertionStore,
    scope: &OutwardScope,
    act_refs: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for act_ref in act_refs {
        let act = store
            .load_act(scope, act_ref)
            .with_context(|| format!("reading the outward disclosure for `{act_ref}`"))?;
        if let Some(act) = act {
            out.insert(act_ref.clone(), act.channel.as_str().to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::TimeZone;

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::delivery::{DeliveryReceipt, DeliveryState};
    use crate::magician_v2::evidence::{OutwardChannel, PrepareOutwardAct};

    /// A fixed clock. Every boundary below is asserted against an exact instant.
    fn t(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, hour, 0, 0)
            .single()
            .expect("a real instant")
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        layout: ArtifactV2Workspace,
        log: DispatchLog,
        ledger: DeliveryLedger,
        scope: DeliveryScope,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        Fixture {
            log: DispatchLog::new(layout.clone()),
            ledger: DeliveryLedger::new(layout.clone()),
            scope: DeliveryScope::new("alpha", "prod"),
            layout,
            _tmp: tmp,
        }
    }

    impl Fixture {
        fn outward_scope(&self) -> OutwardScope {
            OutwardScope::new(self.scope.principal.as_str(), self.scope.workspace.as_str())
        }

        fn disclosure_store(&self) -> OutwardAssertionStore {
            OutwardAssertionStore::new(self.layout.clone())
        }

        /// Prepare a real disclosure on a channel and record its dispatch, so
        /// the act ref the two logs share is the one the store derived.
        fn dispatch(
            &self,
            idempotency_key: &str,
            channel: OutwardChannel,
            at: DateTime<Utc>,
        ) -> String {
            let act = self
                .disclosure_store()
                .prepare(
                    &self.outward_scope(),
                    &PrepareOutwardAct {
                        idempotency_key: idempotency_key.to_string(),
                        program_id: None,
                        engagement_id: None,
                        exact_payload_artifact_ref: format!("artifact-{idempotency_key}"),
                        effective_sender: "owner@example.test".to_string(),
                        intended_audience: vec!["them@example.test".to_string()],
                        channel,
                        consequence_class: "routine".to_string(),
                    },
                    &at.to_rfc3339(),
                )
                .expect("the disclosure prepares");
            self.log
                .record(&self.scope, &act.outward_act_ref, at)
                .expect("the dispatch records");
            act.outward_act_ref
        }

        /// Record a dispatch with NO disclosure behind it.
        fn dispatch_unrecorded(&self, act_ref: &str, at: DateTime<Utc>) {
            self.log
                .record(&self.scope, act_ref, at)
                .expect("the dispatch records");
        }

        fn acknowledge(&self, act_ref: &str, at: DateTime<Utc>) {
            self.ledger
                .reconcile(
                    &self.scope,
                    act_ref,
                    &DeliveryReceipt {
                        provider: "agentmail".to_string(),
                        provider_message_id: format!("pm-{act_ref}"),
                        identity: "them@example.test".to_string(),
                        state: DeliveryState::Accepted,
                        observed_at: at,
                        payload_ref: format!("webhook-{act_ref}"),
                    },
                    at,
                )
                .expect("the receipt reconciles");
        }

        fn report(&self, grace: Duration, now: DateTime<Utc>, limit: usize) -> SilenceReport {
            let scan = scan_silence(&self.log, &self.ledger, &self.scope, grace, now)
                .expect("the scan runs");
            let rails = rails_from_disclosures(
                &self.disclosure_store(),
                &self.outward_scope(),
                &scan.act_refs(),
            )
            .expect("attribution runs");
            attribute(&scan, &rails, limit).expect("the fold runs")
        }
    }

    /// An act nobody acknowledged is named, with how long it has been silent
    /// and which rail it left on.
    ///
    /// Pins the whole point of tier 3: before it, `unreconciled()` had no
    /// caller, so a live send that no provider ever confirmed produced no
    /// signal anywhere and nobody would have noticed for weeks.
    #[test]
    fn an_unacknowledged_act_is_named_with_its_silence_and_its_rail() {
        let f = fixture();
        let act = f.dispatch("mail-1", OutwardChannel::Email, t(1));

        let report = f.report(Duration::hours(1), t(4), 10);

        assert!(report.any_dispatch_recorded);
        assert_eq!(report.scanned, 1);
        assert_eq!(report.acknowledged, 0);
        assert_eq!(report.unacknowledged, 1);
        assert_eq!(report.overdue, 1);
        assert_eq!(report.longest_silence_secs, Some(3 * 3600));
        assert_eq!(report.acts_omitted, 0);
        assert_eq!(
            report.acts,
            vec![SilentAct {
                act_ref: act,
                rail: "email".to_string(),
                dispatched_at: t(1),
                silent_for_secs: 3 * 3600,
            }]
        );
        assert_eq!(
            report.by_rail,
            vec![RailSilence {
                rail: "email".to_string(),
                unacknowledged: 1,
                overdue: 1,
                longest_silence_secs: 3 * 3600,
            }]
        );
        assert!(!report.is_quiet());
    }

    /// An acknowledged act leaves the silence and stays in the denominator.
    ///
    /// The denominator is what makes the count readable. Dropping an
    /// acknowledged act from `scanned` would make one silence out of two read
    /// exactly like one out of a thousand.
    #[test]
    fn an_acknowledged_act_leaves_the_silence_and_stays_in_the_denominator() {
        let f = fixture();
        let quiet = f.dispatch("mail-1", OutwardChannel::Email, t(1));
        let answered = f.dispatch("mail-2", OutwardChannel::Email, t(1));
        f.acknowledge(&answered, t(2));

        let report = f.report(Duration::hours(1), t(4), 10);

        assert_eq!(report.scanned, 2);
        assert_eq!(report.acknowledged, 1);
        assert_eq!(report.unacknowledged, 1);
        assert_eq!(report.overdue, 1);
        assert_eq!(
            report
                .acts
                .iter()
                .map(|act| act.act_ref.as_str())
                .collect::<Vec<_>>(),
            vec![quiet.as_str()]
        );
    }

    /// Counts land on the rail the act's own record names, and a second rail
    /// gets its own line rather than being folded into the first.
    ///
    /// This is the axis the early warning lives on: "email: 40 silent, whatsapp:
    /// 0" is the sentence that says which integration broke. One merged total
    /// would say only that something did.
    #[test]
    fn each_rail_is_counted_on_its_own_line_worst_first() {
        let f = fixture();
        f.dispatch("mail-1", OutwardChannel::Email, t(1));
        f.dispatch("mail-2", OutwardChannel::Email, t(2));
        f.dispatch("wa-1", OutwardChannel::WhatsApp, t(3));

        let report = f.report(Duration::hours(1), t(5), 10);

        assert_eq!(report.unacknowledged, 3);
        assert_eq!(
            report.by_rail,
            vec![
                RailSilence {
                    rail: "email".to_string(),
                    unacknowledged: 2,
                    overdue: 2,
                    longest_silence_secs: 4 * 3600,
                },
                RailSilence {
                    rail: "whatsapp".to_string(),
                    unacknowledged: 1,
                    overdue: 1,
                    longest_silence_secs: 2 * 3600,
                },
            ]
        );
    }

    /// An act nothing could attribute lands in its own bucket, never inside a
    /// named rail's count.
    ///
    /// A rail that has gone quiet must not be able to hide inside another
    /// rail's number, and an act nobody can name must not vanish from the
    /// total: the two halves of "unknown is never permission" for a report.
    #[test]
    fn an_act_with_no_disclosure_is_unattributed_and_never_folded_into_a_rail() {
        let f = fixture();
        f.dispatch("mail-1", OutwardChannel::Email, t(1));
        f.dispatch_unrecorded("orphan-act", t(2));

        let report = f.report(Duration::hours(1), t(5), 10);

        assert_eq!(report.unacknowledged, 2);
        assert_eq!(
            report.by_rail,
            vec![
                RailSilence {
                    rail: "email".to_string(),
                    unacknowledged: 1,
                    overdue: 1,
                    longest_silence_secs: 4 * 3600,
                },
                RailSilence {
                    rail: UNATTRIBUTED.to_string(),
                    unacknowledged: 1,
                    overdue: 1,
                    longest_silence_secs: 3 * 3600,
                },
            ]
        );
        assert_eq!(
            report
                .acts
                .iter()
                .find(|act| act.act_ref == "orphan-act")
                .map(|act| act.rail.as_str()),
            Some(UNATTRIBUTED)
        );
    }

    /// A rail supplied under the reserved name is refused rather than merged
    /// into the bucket that means "we do not know".
    #[test]
    fn a_rail_named_like_the_unattributed_bucket_is_refused() {
        let f = fixture();
        let act = f.dispatch("mail-1", OutwardChannel::Email, t(1));
        let scan = scan_silence(&f.log, &f.ledger, &f.scope, Duration::hours(1), t(4))
            .expect("the scan runs");

        let mut rails = BTreeMap::new();
        rails.insert(act.clone(), UNATTRIBUTED.to_string());
        let error = attribute(&scan, &rails, 10).expect_err("the reserved name is refused");
        assert!(error.to_string().contains(UNATTRIBUTED), "{error}");

        let mut blank = BTreeMap::new();
        blank.insert(act, "   ".to_string());
        let error = attribute(&scan, &blank, 10).expect_err("a blank rail is refused");
        assert!(error.to_string().contains("blank rail"), "{error}");
    }

    /// A scope that has never dispatched anything says so, rather than
    /// reporting a clean sweep of zeroes.
    ///
    /// Pins the vacuous pass. `overdue: 0` over no candidates and `overdue: 0`
    /// over four hundred acknowledged acts are opposite facts that render
    /// identically without the flag, and the first is what a runtime that has
    /// never sent anything — or whose dispatch log was never written — looks
    /// like.
    #[test]
    fn a_scope_with_no_dispatches_says_so_rather_than_reporting_a_clean_sweep() {
        let f = fixture();
        let report = f.report(Duration::hours(1), t(4), 10);

        assert!(!report.any_dispatch_recorded);
        assert_eq!(report.scanned, 0);
        assert_eq!(report.overdue, 0);
        assert!(
            !report.is_quiet(),
            "a scope with no dispatches has nothing overdue and is NOT evidence that delivery \
             works"
        );
    }

    /// Widening the grace moves the overdue count and moves nothing else.
    ///
    /// The idempotence pin. This module records nothing, so a second run
    /// records nothing new, and a decision cannot move because configuration
    /// changed — there is no decision stored to move. The bug this guards
    /// against is the one already found in this codebase: a sweep deriving
    /// ripeness before checking what was already recorded, so widening a window
    /// made a settled act report as open again.
    #[test]
    fn widening_the_grace_moves_only_the_overdue_count() {
        let f = fixture();
        f.dispatch("mail-1", OutwardChannel::Email, t(1));
        f.dispatch("mail-2", OutwardChannel::Email, t(3));

        let tight = f.report(Duration::hours(1), t(4), 10);
        assert_eq!(tight.overdue, 2, "both are silent for at least an hour");

        let wide = f.report(Duration::hours(2), t(4), 10);
        assert_eq!(
            wide.overdue, 1,
            "only the older one clears a two-hour grace"
        );
        assert_eq!(wide.unacknowledged, tight.unacknowledged);
        assert_eq!(wide.scanned, tight.scanned);
        assert_eq!(wide.acknowledged, tight.acknowledged);
        assert_eq!(wide.longest_silence_secs, tight.longest_silence_secs);

        // And running the tight report again lands on the same answer: nothing
        // was recorded by the first pass that the second could read back.
        assert_eq!(f.report(Duration::hours(1), t(4), 10), tight);
    }

    /// The grace boundary is inclusive, and it is the ledger's rule rather than
    /// a second copy of it here.
    #[test]
    fn an_act_silent_for_exactly_the_grace_is_overdue() {
        let f = fixture();
        f.dispatch("mail-1", OutwardChannel::Email, t(1));

        assert_eq!(f.report(Duration::hours(3), t(4), 10).overdue, 1);
        assert_eq!(f.report(Duration::hours(4), t(4), 10).overdue, 0);
    }

    /// An act dispatched ahead of the clock is counted apart from both
    /// acknowledged and unacknowledged.
    ///
    /// Clock skew must not erase a send. The zero-grace sweep cannot see an act
    /// whose silence is negative, so without its own count the act would fall
    /// out of `unacknowledged` and be subtracted into `acknowledged` — a
    /// send silently reported as confirmed by nothing but a fast clock.
    #[test]
    fn an_act_dispatched_ahead_of_the_clock_is_counted_apart_from_both() {
        let f = fixture();
        f.dispatch("mail-future", OutwardChannel::Email, t(9));

        let report = f.report(Duration::hours(1), t(4), 10);

        assert_eq!(report.scanned, 1);
        assert_eq!(report.dispatched_ahead_of_the_clock, 1);
        assert_eq!(report.unacknowledged, 0);
        assert_eq!(
            report.acknowledged, 0,
            "a future-dated dispatch must never be subtracted into the acknowledged count"
        );
    }

    /// A truncated act list says how many it left out.
    #[test]
    fn a_capped_act_list_reports_what_it_omitted_and_a_zero_cap_is_refused() {
        let f = fixture();
        f.dispatch("mail-1", OutwardChannel::Email, t(1));
        f.dispatch("mail-2", OutwardChannel::Email, t(2));
        f.dispatch("mail-3", OutwardChannel::Email, t(3));

        let report = f.report(Duration::hours(1), t(5), 2);
        assert_eq!(report.overdue, 3);
        assert_eq!(report.acts.len(), 2);
        assert_eq!(report.acts_omitted, 1);
        assert!(
            report.acts[0].silent_for_secs > report.acts[1].silent_for_secs,
            "the oldest silence is named first"
        );

        let scan = scan_silence(&f.log, &f.ledger, &f.scope, Duration::hours(1), t(5))
            .expect("the scan runs");
        let error = attribute(&scan, &BTreeMap::new(), 0).expect_err("a zero cap is refused");
        assert!(error.to_string().contains("at least one act"), "{error}");
    }

    /// A scope carrying the id separator is refused before anything is read.
    ///
    /// The scope reaches this module from an HTTP header, so it is a caller
    /// string. Checking it only inside the ledger would leave one path
    /// unguarded: a crafted scope whose dispatch log happens to be absent
    /// returns the "nothing dispatched" answer without the ledger ever being
    /// asked.
    #[test]
    fn a_scope_holding_the_field_separator_is_refused_before_any_read() {
        let f = fixture();
        let error = scan_silence(
            &f.log,
            &f.ledger,
            &DeliveryScope::new("al\u{1f}pha", "prod"),
            Duration::hours(1),
            t(4),
        )
        .expect_err("U+001F is refused");
        assert!(error.to_string().contains("U+001F"), "{error}");
    }

    /// A negative grace is refused before anything is read.
    #[test]
    fn a_negative_grace_is_refused() {
        let f = fixture();
        let error = scan_silence(&f.log, &f.ledger, &f.scope, Duration::hours(-1), t(4))
            .expect_err("a negative grace is refused");
        assert!(error.to_string().contains("negative grace"), "{error}");
    }

    /// An unreadable dispatch log propagates instead of reading as silence-free.
    ///
    /// A report that answered "nothing is unacknowledged" because it could not
    /// read the log would manufacture exactly the confidence this module exists
    /// to withhold.
    #[test]
    fn an_unreadable_dispatch_log_propagates_rather_than_reading_as_quiet() {
        let f = fixture();
        f.dispatch("mail-1", OutwardChannel::Email, t(1));

        let path = f
            .layout
            .scope_root(&f.scope.principal, &f.scope.workspace)
            .join("delivery")
            .join("dispatched.jsonl");
        std::fs::write(&path, b"not json at all\n").expect("corrupt the log");

        let error = scan_silence(&f.log, &f.ledger, &f.scope, Duration::hours(1), t(4))
            .expect_err("an unreadable dispatch log is an error");
        assert!(error.to_string().contains("dispatch log"), "{error}");
    }
}
