//! The maturity policy's caller — assembling the acts that went quiet, and
//! turning their silence into recorded fact.
//!
//! Plan: `docs/plans/2026-08-07-opc-outcome-learning.md` §2, phase 2.
//!
//! [`super::maturity::mature_silences`] decides *when silence becomes a fact*
//! and [`super::feeders::awaiting_outcomes`] assembles its input, and **nothing
//! runs either**. That is not a wiring gap, it is the failure the phase exists
//! to prevent stated backwards: with no sweep, the only observations that ever
//! reach the store are the ones somebody bothered to record — which are the
//! replies. A sample containing only the counterparties who answered is the
//! most flattering dataset available and the least useful one, and every later
//! cohort comparison inherits it.
//!
//! This module is the caller, and it is three layers rather than one:
//!
//! - [`collect_acts`] — reads the recorded outward acts a caller names and
//!   joins each to the cohort key it was performed under. **Reads the act
//!   store, writes nothing.**
//! - [`sweep_matured_silences`] — runs the feeder and the policy over acts the
//!   caller already holds, and records what matured. **The only function here
//!   that writes.**
//! - [`run_maturity_sweep`] — the two together, for a caller that just wants
//!   the sweep to happen.
//!
//! Splitting them is what lets the assembly be tested against a real act store
//! with no outcome store in sight, and the maturation be tested against a real
//! outcome store with no acts on disk.
//!
//! # Generic first: a fundraising flow is one consumer
//!
//! Nothing in the public API names a domain. The primitive is *"we did
//! something outward, a waiting period elapsed, and nothing came back"* —
//! identical for a support reply, a supplier chase, a candidate loop or a
//! pitch. The cohort key is supplied by whoever decided which variant to use,
//! because that is the deciding subsystem's fact and not the carrier's.
//!
//! # Idempotent, and terminal decisions stay terminal
//!
//! Running this twice must not double-record and must not move a decision
//! already made. Three separate mechanisms hold that, and they are layered
//! deliberately:
//!
//! 1. [`mature_silences`] short-circuits an act that already carries a
//!    `Silent` observation as [`NotMatured::AlreadyRecorded`] — **before**
//!    reaching the store, so a second sweep does not even re-derive the
//!    maturity instant. This is what makes the decision terminal: a policy
//!    whose window was widened between sweeps would otherwise compute a later
//!    `matured_at` for an act whose silence was already recorded, and append a
//!    correction that moved a settled fact.
//! 2. [`mature_silences`] re-reads each act's own observations rather than
//!    trusting this module's list, so an act that was answered between sweeps
//!    comes back [`NotMatured::AlreadyAnswered`] and never matures.
//! 3. [`OutcomeStore::record`] is idempotent on
//!    `(act, variant, version, label)` underneath both, so even a caller that
//!    bypassed the first two cannot inflate the sample.
//!
//! # Fail closed
//!
//! - **An act the store does not hold is refused, never swept.** A caller
//!   naming an act ref that resolves to nothing is a caller whose list is
//!   wrong, and recording *"nobody replied"* about an act we cannot read is a
//!   claim with no basis. It is reported in
//!   [`NotCollected::NoSuchAct`], not skipped quietly.
//! - **An identical binding replays; a changed one is an error.** The same act
//!   named twice with the same cohort key is one act. The same act named twice
//!   with different keys would put one outcome in two cohorts and make the
//!   comparison agree with whichever copy was read last.
//! - **The two scopes must name the same tenant.** Reading one principal's
//!   acts while recording another's observations is not a configuration a
//!   sweep should be able to express, so it is refused rather than trusted.
//! - **Counts, never rates.** The report carries how many matured, how many
//!   were still open and how many were already settled. No ratio is computed
//!   from them; a "maturity rate" would read as progress whichever way it
//!   moved.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use magician::magician_v2::evidence::outward_assertions::{OutwardAssertionStore, OutwardScope};
use magician::magician_v2::work_context::WorkContextKind;

use super::feeders::{awaiting_outcomes, NotAwaitingAct, OutwardActCohort};
use super::maturity::{mature_silences, MaturityPolicy, NotMatured};
use super::store::{OutcomeScope, OutcomeStore};
use super::types::Confounder;

/// One act a caller believes is awaiting an outcome, with the cohort key it was
/// performed under.
///
/// The cohort key is **supplied**, never read from the act. An act records what
/// went out and when; it does not record which variant version was live when it
/// did, because that is the deciding subsystem's fact. Supplying it here is
/// what makes the resulting observation comparable to anything at all — without
/// it a sample can only ever agree with itself, which is the difference
/// between learning and self-confirmation the whole plan turns on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActCohortBinding {
    pub act_ref: String,
    pub variant_ref: String,
    /// The cohort key: which variant version was active when this act happened.
    pub variant_version: String,
    /// What was true about the situation that is not the thing being tested.
    pub confounders: Vec<Confounder>,
}

impl ActCohortBinding {
    pub fn new(
        act_ref: impl Into<String>,
        variant_ref: impl Into<String>,
        variant_version: impl Into<String>,
    ) -> Self {
        Self {
            act_ref: act_ref.into(),
            variant_ref: variant_ref.into(),
            variant_version: variant_version.into(),
            confounders: Vec::new(),
        }
    }

    pub fn with_confounders(mut self, confounders: Vec<Confounder>) -> Self {
        self.confounders = confounders;
        self
    }
}

/// Why a named act did not reach the sweep.
///
/// Both variants are refusals rather than omissions: a sweep that dropped an
/// unreadable act quietly would report a partial pass as a complete one, and
/// the whole point of the phase is that the sample is not quietly filtered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotCollected {
    /// The binding named no act at all.
    UnnamedAct,
    /// The act store holds nothing under this ref.
    ///
    /// Refused, never treated as "then there is no outcome to wait for". An act
    /// nobody recorded is an act we know nothing about, and *"they did not
    /// reply"* about something we cannot read is a claim with no basis —
    /// unknown is never permission.
    NoSuchAct,
}

/// One act that did not reach the sweep, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncollectedAct {
    pub act_ref: String,
    pub reason: NotCollected,
}

/// What one assembly produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CollectedActs {
    /// Acts joined to their cohort key, in the order the bindings named them.
    pub acts: Vec<OutwardActCohort>,
    pub not_collected: Vec<UncollectedAct>,
}

impl CollectedActs {
    pub fn collected_count(&self) -> usize {
        self.acts.len()
    }
}

/// Every act ref the store has filed under one piece of work.
///
/// Takes a [`WorkContextKind`] rather than an engagement id, and reads the
/// axis from that kind's own wire token. **This is the generic axis**: a
/// programme, an engagement, and any third kind of work added later are all
/// reachable through it without this function being edited, which is the
/// property that decides whether a second flow can exist at all.
///
/// It was engagement-only until now, against a store that indexed
/// `engagement_id` and nothing else. Both halves had to move: an axis nobody
/// files under returns an empty index, and an empty index reads exactly like a
/// piece of work with no acts — so a sweep over a programme would have found
/// nothing and reported a clean pass.
///
/// Order is the index's, which is the order the acts were prepared in.
/// Duplicates are already collapsed by the store's own read.
///
/// An empty result means *"this work has no acts filed"*, never *"this work is
/// settled"*. The distinction matters because an empty index is also what a
/// work id nobody ever recorded against looks like.
pub fn act_refs_for_work(
    store: &OutwardAssertionStore,
    scope: &OutwardScope,
    work: &WorkContextKind,
) -> Result<Vec<String>> {
    // The same guard the carrier applies, applied before a blank or
    // separator-carrying id reaches a path derivation: `stable_id("")` is a
    // perfectly good hash, so a blank work id would address one real index file
    // shared by every blank-id caller.
    work.guard_id()
        .map_err(|message| anyhow::anyhow!(message))?;
    store
        .index_entries(scope, work.kind_token(), work.id())
        .with_context(|| {
            format!(
                "listing outward acts for {} `{}`",
                work.kind_token(),
                work.id()
            )
        })
}

/// [`act_refs_for_work`], for a caller that already holds an engagement id.
///
/// Kept as a named wrapper rather than as the primitive: the primitive taking
/// an engagement is what made every other kind of work unreachable, and a
/// wrapper cannot make that mistake again because it has nowhere to put the
/// axis.
pub fn act_refs_for_engagement(
    store: &OutwardAssertionStore,
    scope: &OutwardScope,
    engagement_id: &str,
) -> Result<Vec<String>> {
    act_refs_for_work(
        store,
        scope,
        &WorkContextKind::Engagement(engagement_id.to_string()),
    )
}

/// Load each named act and join it to its cohort key.
///
/// Reads the act store; writes nothing. What it refuses:
///
/// - An **unnamed** act — a blank ref addresses nothing.
/// - An act the store does not hold, as [`NotCollected::NoSuchAct`].
/// - The same act named twice with **different** cohort keys, as an error
///   rather than a refusal: it is not one act that cannot be swept, it is a
///   caller assembling its input wrongly, and silently resolving it to
///   whichever copy came last would put one act's outcome in two cohorts.
///
/// An identical repeat of a binding is one act, matching
/// [`awaiting_outcomes`]'s own rule so the two cannot disagree about how many
/// acts a duplicated list describes.
///
/// Whether an act is actually *awaiting* anything — prepared and never sent,
/// failed, retracted, undated — is deliberately **not** decided here.
/// [`awaiting_outcomes`] owns that, and a second filter at this layer would be
/// an undocumented one nobody could see in the report.
pub fn collect_acts(
    store: &OutwardAssertionStore,
    scope: &OutwardScope,
    bindings: &[ActCohortBinding],
) -> Result<CollectedActs> {
    let mut unique: BTreeMap<&str, &ActCohortBinding> = BTreeMap::new();
    let mut order: Vec<&ActCohortBinding> = Vec::new();
    let mut collected = CollectedActs::default();

    for binding in bindings {
        let act_ref = binding.act_ref.trim();
        if act_ref.is_empty() {
            collected.not_collected.push(UncollectedAct {
                act_ref: binding.act_ref.clone(),
                reason: NotCollected::UnnamedAct,
            });
            continue;
        }
        match unique.get(act_ref) {
            Some(held) if *held == binding => continue,
            Some(_) => anyhow::bail!(
                "act `{act_ref}` was bound twice with different cohort keys: an identical repeat \
                 is one act, but a changed key would put one act's outcome in two cohorts and \
                 make every later comparison agree with whichever copy was read last"
            ),
            None => {
                unique.insert(act_ref, binding);
                order.push(binding);
            },
        }
    }

    for binding in order {
        let act_ref = binding.act_ref.trim();
        let Some(act) = store
            .load_act(scope, act_ref)
            .with_context(|| format!("loading outward act `{act_ref}` for the maturity sweep"))?
        else {
            collected.not_collected.push(UncollectedAct {
                act_ref: binding.act_ref.clone(),
                reason: NotCollected::NoSuchAct,
            });
            continue;
        };
        collected.acts.push(OutwardActCohort {
            act,
            variant_ref: binding.variant_ref.clone(),
            variant_version: binding.variant_version.clone(),
            confounders: binding.confounders.clone(),
        });
    }

    Ok(collected)
}

/// What one sweep did, in counts and named refusals.
///
/// Every input is accounted for exactly once across the four lists, so a caller
/// can always answer *"why did this sweep see less than I expected"* rather
/// than staring at a smaller number than it thought it had.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MaturitySweepReport {
    /// Acts named to the sweep, after identical repeats collapsed.
    pub considered: usize,
    /// Named acts that never reached the feeder.
    pub not_collected: Vec<UncollectedAct>,
    /// Acts that reached the feeder and are not awaiting an outcome — never
    /// dispatched, settled, undated. Silence after any of those is a fact about
    /// the carrier, not about a counterparty.
    pub not_awaiting: Vec<NotAwaitingAct>,
    /// Acts whose silence was recorded by this sweep.
    pub matured: Vec<String>,
    /// Acts the policy declined to mature, each with its reason.
    pub skipped: Vec<(String, NotMatured)>,
}

impl MaturitySweepReport {
    /// How many silences this sweep turned into fact.
    ///
    /// A count, never a rate. *"Four matured, nine still open"* is a fact an
    /// owner can act on; a percentage hides which acts moved and reads as
    /// progress whichever direction it goes.
    pub fn matured_count(&self) -> usize {
        self.matured.len()
    }

    /// Acts whose window has not closed. Not a problem — the ordinary state of
    /// a recent act.
    pub fn still_open_count(&self) -> usize {
        self.skipped
            .iter()
            .filter(|(_, reason)| matches!(reason, NotMatured::StillOpen { .. }))
            .count()
    }

    /// Acts already decided — silence recorded, or something came back.
    ///
    /// This is the number a second run of the same sweep reports where the
    /// first reported [`matured_count`](Self::matured_count): terminal
    /// decisions do not resurrect, so a repeat converges here rather than
    /// recording anything again.
    pub fn already_settled_count(&self) -> usize {
        self.skipped
            .iter()
            .filter(|(_, reason)| {
                matches!(
                    reason,
                    NotMatured::AlreadyRecorded | NotMatured::AlreadyAnswered { .. }
                )
            })
            .count()
    }

    /// Whether this sweep wrote anything.
    ///
    /// *"Nothing to do on these facts"*, never *"everything is answered"* — an
    /// empty sweep over an empty input list is the vacuous case, and reading it
    /// as health is the bug the whole module is arranged against.
    pub fn recorded_nothing(&self) -> bool {
        self.matured.is_empty()
    }
}

/// Run the feeder and the maturity policy over acts the caller already holds,
/// and record what matured.
///
/// The only function in this module that writes. It is deliberately thin: which
/// acts are awaiting anything is [`awaiting_outcomes`]'s judgement, when a
/// window closes is [`MaturityPolicy`]'s, and whether something already came
/// back is re-read from the act's own observations inside
/// [`mature_silences`]. Nothing is decided here that either of them could
/// decide, so there is one place to read each rule.
///
/// `now` bounds what is knowable, inclusively — an act whose window closes at
/// this very instant has matured, matching every other deadline in this
/// codebase.
///
/// Safe to run twice: see the module note on the three layers of idempotency.
/// A second run over the same acts reports them in
/// [`already_settled_count`](MaturitySweepReport::already_settled_count) and
/// records nothing.
pub fn sweep_matured_silences(
    outcomes: &OutcomeStore,
    outcome_scope: &OutcomeScope,
    acts: &[OutwardActCohort],
    policy: &MaturityPolicy,
    now: DateTime<Utc>,
) -> Result<MaturitySweepReport> {
    let feed = awaiting_outcomes(acts, now)?;
    let maturation = mature_silences(outcomes, outcome_scope, &feed.awaiting, policy, now)
        .context("maturing the silences this sweep found")?;

    Ok(MaturitySweepReport {
        considered: feed.awaiting.len() + feed.not_awaiting.len(),
        not_collected: Vec::new(),
        not_awaiting: feed.not_awaiting,
        matured: maturation.matured,
        skipped: maturation.skipped,
    })
}

/// Assemble from the act store and sweep, in one call.
///
/// # The two scopes must name the same tenant
///
/// Refused rather than trusted. The acts are read under `outward_scope` and the
/// observations are written under `outcome_scope`, so a mismatch would record
/// one principal's silence in another principal's cohort — a wrong answer
/// shaped exactly like a right one, since both stores would report success.
/// Nothing legitimate needs the two to differ.
pub fn run_maturity_sweep(
    outward: &OutwardAssertionStore,
    outward_scope: &OutwardScope,
    outcomes: &OutcomeStore,
    outcome_scope: &OutcomeScope,
    bindings: &[ActCohortBinding],
    policy: &MaturityPolicy,
    now: DateTime<Utc>,
) -> Result<MaturitySweepReport> {
    if outward_scope.principal != outcome_scope.principal
        || outward_scope.workspace != outcome_scope.workspace
    {
        anyhow::bail!(
            "this sweep would read acts as `{}/{}` and record outcomes as `{}/{}`: one tenant's \
             silence recorded in another's cohort is a wrong answer both stores would report as \
             a success",
            outward_scope.principal,
            outward_scope.workspace,
            outcome_scope.principal,
            outcome_scope.workspace,
        );
    }

    let collected = collect_acts(outward, outward_scope, bindings)?;
    let mut report = sweep_matured_silences(outcomes, outcome_scope, &collected.acts, policy, now)?;
    // The named acts that never reached the feeder are part of what this sweep
    // considered — folding them in here rather than leaving them behind is what
    // makes every input accounted for exactly once.
    report.considered += collected.not_collected.len();
    report.not_collected = collected.not_collected;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::evidence::outward_assertions::{OutwardChannel, PrepareOutwardAct};

    use super::super::feeders::NotAwaiting;
    use super::super::types::{DeliveryState, OutcomeLabel, RecordOutcome};
    use super::*;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, day, hour, 0, 0).unwrap()
    }

    struct Bench {
        _dir: tempfile::TempDir,
        outward: OutwardAssertionStore,
        outward_scope: OutwardScope,
        outcomes: OutcomeStore,
        outcome_scope: OutcomeScope,
    }

    fn bench() -> Bench {
        let dir = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(dir.path());
        Bench {
            _dir: dir,
            outward: OutwardAssertionStore::new(layout.clone()),
            outward_scope: OutwardScope::new("owner", "work"),
            outcomes: OutcomeStore::new(layout),
            outcome_scope: OutcomeScope::new("owner", "work"),
        }
    }

    /// Prepare an act and drive it to delivered at `dispatched`.
    fn delivered_act(bench: &Bench, key: &str, dispatched: DateTime<Utc>) -> String {
        let act = bench
            .outward
            .prepare(
                &bench.outward_scope,
                &PrepareOutwardAct {
                    idempotency_key: key.to_string(),
                    program_id: None,
                    engagement_id: Some("engagement-1".to_string()),
                    exact_payload_artifact_ref: format!("artifact-{key}@1"),
                    effective_sender: "sender@example.test".to_string(),
                    intended_audience: vec!["reader@example.test".to_string()],
                    channel: OutwardChannel::Email,
                    consequence_class: "bounded_communication".to_string(),
                },
                &at(1, 0).to_rfc3339(),
            )
            .expect("prepare");
        bench
            .outward
            .mark_dispatching(
                &bench.outward_scope,
                &act.outward_act_ref,
                &dispatched.to_rfc3339(),
            )
            .expect("dispatching");
        bench
            .outward
            .record_delivered(
                &bench.outward_scope,
                &act.outward_act_ref,
                &dispatched.to_rfc3339(),
            )
            .expect("delivered");
        act.outward_act_ref
    }

    fn policy() -> MaturityPolicy {
        MaturityPolicy::new(Duration::days(14)).expect("a positive window")
    }

    /// The failure the whole phase exists for: nothing ran, so the only
    /// observations that ever existed were the replies.
    ///
    /// A sweep over a delivered act whose window has closed must record
    /// `Silent` — and it must record it against the cohort key the caller
    /// supplied, or the observation cannot be compared with anything.
    #[test]
    fn a_closed_window_on_a_delivered_act_becomes_a_recorded_silence() {
        let bench = bench();
        let act_ref = delivered_act(&bench, "act-1", at(1, 0));
        let bindings = vec![ActCohortBinding::new(&act_ref, "opening-line", "v3")];

        let report = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &bench.outcome_scope,
            &bindings,
            &policy(),
            at(20, 0),
        )
        .expect("a sweep");

        assert_eq!(report.matured_count(), 1);
        assert_eq!(report.matured, vec![act_ref.clone()]);
        assert_eq!(report.considered, 1);
        assert!(report.not_collected.is_empty());
        assert!(report.not_awaiting.is_empty());

        let observations = bench
            .outcomes
            .observations_for_act(&bench.outcome_scope, &act_ref)
            .expect("read back");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].label, OutcomeLabel::Silent);
        assert_eq!(observations[0].variant_ref, "opening-line");
        assert_eq!(observations[0].variant_version, "v3");
        assert_eq!(observations[0].delivery_state, DeliveryState::Delivered);
        assert_eq!(observations[0].matured_at, Some(at(15, 0)));
        // The window closed on the 15th; the sweep ran on the 20th. The
        // recorded maturity instant is the window's, never the sweep clock's —
        // a sweep that ran late must not report the silence as newer than it is.
        assert_eq!(observations[0].observed_at, at(20, 0));
    }

    /// Running the sweep twice must not double-record, and the second run must
    /// not move the decision the first one made.
    ///
    /// The subtle half is the policy change: widening the window between sweeps
    /// derives a LATER maturity instant for the same act. Without the
    /// already-recorded short-circuit the store would append that as a
    /// correction, silently moving a settled fact — "we concluded they were
    /// silent as of the 15th" would become "as of the 29th" because somebody
    /// edited configuration.
    #[test]
    fn a_second_sweep_records_nothing_and_moves_nothing() {
        let bench = bench();
        let act_ref = delivered_act(&bench, "act-1", at(1, 0));
        let bindings = vec![ActCohortBinding::new(&act_ref, "opening-line", "v3")];

        let first = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &bench.outcome_scope,
            &bindings,
            &policy(),
            at(20, 0),
        )
        .expect("first sweep");
        assert_eq!(first.matured_count(), 1);

        let widened = MaturityPolicy::new(Duration::days(28)).expect("a positive window");
        let second = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &bench.outcome_scope,
            &bindings,
            &widened,
            at(21, 0),
        )
        .expect("second sweep");

        assert_eq!(second.matured_count(), 0);
        assert_eq!(second.already_settled_count(), 1);
        assert_eq!(second.skipped[0].0, act_ref);
        assert_eq!(second.skipped[0].1, NotMatured::AlreadyRecorded);

        let observations = bench
            .outcomes
            .observations_for_act(&bench.outcome_scope, &act_ref)
            .expect("read back");
        assert_eq!(observations.len(), 1, "the sample must not have grown");
        assert_eq!(
            observations[0].matured_at,
            Some(at(15, 0)),
            "a settled maturity decision must not move because the window was widened"
        );
    }

    /// An act somebody answered must never mature into silence, however long
    /// the window has been closed.
    ///
    /// A rejection is the case that makes this matter: it is not engagement,
    /// but the counterparty answered. Maturing it would record that they both
    /// replied and ignored us.
    #[test]
    fn an_answered_act_never_matures_into_silence() {
        let bench = bench();
        let act_ref = delivered_act(&bench, "act-1", at(1, 0));
        bench
            .outcomes
            .record(
                &bench.outcome_scope,
                &RecordOutcome {
                    engagement_id: Some("engagement-1".to_string()),
                    program_id: None,
                    act_ref: act_ref.clone(),
                    variant_ref: "opening-line".to_string(),
                    variant_version: "v3".to_string(),
                    label: OutcomeLabel::Rejected,
                    delivery_state: DeliveryState::Delivered,
                    matured_at: None,
                    confounders: Vec::new(),
                },
                at(3, 0),
            )
            .expect("a rejection");

        let report = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &bench.outcome_scope,
            &[ActCohortBinding::new(&act_ref, "opening-line", "v3")],
            &policy(),
            at(20, 0),
        )
        .expect("a sweep");

        assert_eq!(report.matured_count(), 0);
        assert_eq!(
            report.skipped,
            vec![(
                act_ref.clone(),
                NotMatured::AlreadyAnswered {
                    label: OutcomeLabel::Rejected
                }
            )]
        );
        let observations = bench
            .outcomes
            .observations_for_act(&bench.outcome_scope, &act_ref)
            .expect("read back");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].label, OutcomeLabel::Rejected);
    }

    /// An act the store does not hold is refused, not skipped.
    ///
    /// A sweep that dropped it quietly would report a partial pass as a
    /// complete one — and recording "nobody replied" about an act we cannot
    /// read is a claim with no basis at all.
    #[test]
    fn an_act_the_store_does_not_hold_is_refused_by_name() {
        let bench = bench();
        let report = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &bench.outcome_scope,
            &[
                ActCohortBinding::new("act-nobody-recorded", "opening-line", "v3"),
                ActCohortBinding::new("   ", "opening-line", "v3"),
            ],
            &policy(),
            at(20, 0),
        )
        .expect("a sweep");

        assert_eq!(report.matured_count(), 0);
        assert_eq!(report.considered, 2);
        assert_eq!(
            report.not_collected,
            vec![
                UncollectedAct {
                    act_ref: "   ".to_string(),
                    reason: NotCollected::UnnamedAct,
                },
                UncollectedAct {
                    act_ref: "act-nobody-recorded".to_string(),
                    reason: NotCollected::NoSuchAct,
                },
            ]
        );
    }

    /// A prepared act never left, so nothing about it is silence.
    ///
    /// Maturing it would record that a counterparty ignored a message that was
    /// never sent — and the act still has to appear in the report, or a sweep
    /// that saw five acts and swept none would look like a sweep that found
    /// nothing to do.
    #[test]
    fn an_act_that_never_left_is_reported_rather_than_matured() {
        let bench = bench();
        let act = bench
            .outward
            .prepare(
                &bench.outward_scope,
                &PrepareOutwardAct {
                    idempotency_key: "act-prepared".to_string(),
                    program_id: None,
                    engagement_id: Some("engagement-1".to_string()),
                    exact_payload_artifact_ref: "artifact-prepared@1".to_string(),
                    effective_sender: "sender@example.test".to_string(),
                    intended_audience: vec!["reader@example.test".to_string()],
                    channel: OutwardChannel::Email,
                    consequence_class: "bounded_communication".to_string(),
                },
                &at(1, 0).to_rfc3339(),
            )
            .expect("prepare");

        let report = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &bench.outcome_scope,
            &[ActCohortBinding::new(
                &act.outward_act_ref,
                "opening-line",
                "v3",
            )],
            &policy(),
            at(20, 0),
        )
        .expect("a sweep");

        assert_eq!(report.matured_count(), 0);
        assert_eq!(report.considered, 1);
        assert_eq!(report.not_awaiting.len(), 1);
        assert_eq!(report.not_awaiting[0].act_ref, act.outward_act_ref);
        assert!(matches!(
            report.not_awaiting[0].reason,
            NotAwaiting::NothingLeft { .. }
        ));
        assert!(bench
            .outcomes
            .observations_for_act(&bench.outcome_scope, &act.outward_act_ref)
            .expect("read back")
            .is_empty());
    }

    /// Two cohort keys for one act is a caller error, not a value to pick from.
    ///
    /// Resolving it to whichever copy was read last would put one act's outcome
    /// in two cohorts, and the before/after comparison that separates learning
    /// from self-confirmation would then be run against a sample that disagrees
    /// with itself.
    #[test]
    fn one_act_bound_to_two_cohort_keys_is_an_error() {
        let bench = bench();
        let act_ref = delivered_act(&bench, "act-1", at(1, 0));

        let identical = collect_acts(
            &bench.outward,
            &bench.outward_scope,
            &[
                ActCohortBinding::new(&act_ref, "opening-line", "v3"),
                ActCohortBinding::new(&act_ref, "opening-line", "v3"),
            ],
        )
        .expect("an identical repeat is one act");
        assert_eq!(identical.collected_count(), 1);

        let error = collect_acts(
            &bench.outward,
            &bench.outward_scope,
            &[
                ActCohortBinding::new(&act_ref, "opening-line", "v3"),
                ActCohortBinding::new(&act_ref, "opening-line", "v4"),
            ],
        )
        .expect_err("a changed cohort key");
        assert!(
            error.to_string().contains("two cohorts"),
            "the error must say what breaks: {error}"
        );
    }

    /// Reading one tenant's acts while recording another's outcomes is refused.
    ///
    /// Both stores would report success, so nothing downstream could tell that
    /// a principal's cohort had been filled with somebody else's silence.
    #[test]
    fn a_sweep_across_two_tenants_is_refused() {
        let bench = bench();
        let error = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &OutcomeScope::new("someone-else", "work"),
            &[],
            &policy(),
            at(20, 0),
        )
        .expect_err("two tenants");
        assert!(
            error.to_string().contains("another's cohort"),
            "the error must name the confusion: {error}"
        );
    }

    /// The engagement index is how a sweep discovers acts, and it must return
    /// what was actually filed.
    ///
    /// An empty answer here reads exactly like an engagement with no acts, so a
    /// wrong axis name would present "we swept nothing" as "there was nothing
    /// to sweep" — the vacuous pass this codebase refuses everywhere.
    #[test]
    fn the_engagement_index_enumerates_the_acts_a_sweep_should_consider() {
        let bench = bench();
        let first = delivered_act(&bench, "act-1", at(1, 0));
        let second = delivered_act(&bench, "act-2", at(2, 0));

        let found = act_refs_for_engagement(&bench.outward, &bench.outward_scope, "engagement-1")
            .expect("index read");
        assert_eq!(found, vec![first, second]);

        assert!(act_refs_for_engagement(
            &bench.outward,
            &bench.outward_scope,
            "engagement-nobody-used"
        )
        .expect("index read")
        .is_empty());
    }

    /// An act performed inside a **programme** is findable by that programme.
    ///
    /// The failure this pins is the one that made the whole sweep unreachable
    /// for every flow but one: the act store indexed `engagement_id` and never
    /// `program_id`, and this lookup read a hardcoded engagement axis. A run
    /// whose audience was a programme set `program_id` and left
    /// `engagement_id` empty, so its acts were filed under artifact and
    /// recipient only — and a sweep enumerating the programme found zero acts,
    /// which reads exactly like a programme that never said anything. Both
    /// halves had to move, so this asserts the joined result rather than either
    /// one.
    #[test]
    fn an_act_performed_inside_a_programme_is_found_under_the_programme_axis() {
        let bench = bench();
        let act = bench
            .outward
            .prepare(
                &bench.outward_scope,
                &PrepareOutwardAct {
                    idempotency_key: "programme-act".to_string(),
                    program_id: Some("program-7".to_string()),
                    // A PROGRAMME-scoped act, written directly. It is no longer
                    // the shape `run_state::submit` produces: an `AudienceRef`
                    // id and a work id are different id spaces — the audience
                    // kind `Program` names a counterparty-register label, not a
                    // programme — so `submit` now writes neither work field and
                    // files by audience instead. What this test pins is the
                    // SWEEP: an act genuinely performed inside a programme is
                    // found under the programme axis. That is unchanged, and it
                    // is still reached by a dispatch-path act carrying a
                    // `WorkContextKind::Program`.
                    engagement_id: None,
                    exact_payload_artifact_ref: "artifact-programme@1".to_string(),
                    effective_sender: "sender@example.test".to_string(),
                    intended_audience: vec!["cohort@example.test".to_string()],
                    channel: OutwardChannel::Email,
                    consequence_class: "bounded_communication".to_string(),
                },
                &at(1, 0).to_rfc3339(),
            )
            .expect("prepare");

        let found = act_refs_for_work(
            &bench.outward,
            &bench.outward_scope,
            &WorkContextKind::Program("program-7".to_string()),
        )
        .expect("index read");
        assert_eq!(found, vec![act.outward_act_ref.clone()]);

        // And it is not filed under an engagement of the same id: the axis is
        // the kind's own token, so `engagement:program-7` and `program:program-7`
        // cannot merge.
        assert_eq!(
            act_refs_for_engagement(&bench.outward, &bench.outward_scope, "program-7")
                .expect("index read"),
            Vec::<String>::new()
        );
    }

    /// A blank work id is refused before it reaches a path derivation.
    ///
    /// `blake3("")` is a perfectly good hash, so a blank id addresses one real
    /// index file — shared by every blank-id caller. Answering from it would
    /// hand one flow another's acts, and answering "no acts" would be a
    /// vacuous pass.
    #[test]
    fn a_blank_work_id_is_refused_rather_than_hashed() {
        let bench = bench();
        let refused = act_refs_for_work(
            &bench.outward,
            &bench.outward_scope,
            &WorkContextKind::Engagement("   ".to_string()),
        );
        assert!(
            refused.is_err(),
            "a blank work id must not address a record"
        );
    }

    /// A window that has not closed is reported as open, never as silence.
    ///
    /// Before maturity, silence is indistinguishable from "not yet", and the
    /// sweep must say which of the two it is rather than filtering the act out.
    #[test]
    fn an_open_window_is_reported_rather_than_dropped() {
        let bench = bench();
        let act_ref = delivered_act(&bench, "act-1", at(1, 0));

        let report = run_maturity_sweep(
            &bench.outward,
            &bench.outward_scope,
            &bench.outcomes,
            &bench.outcome_scope,
            &[ActCohortBinding::new(&act_ref, "opening-line", "v3")],
            &policy(),
            at(10, 0),
        )
        .expect("a sweep");

        assert_eq!(report.matured_count(), 0);
        assert_eq!(report.still_open_count(), 1);
        assert_eq!(
            report.skipped,
            vec![(
                act_ref.clone(),
                NotMatured::StillOpen {
                    matures_at: at(15, 0)
                }
            )]
        );
        assert!(report.recorded_nothing());
    }
}
