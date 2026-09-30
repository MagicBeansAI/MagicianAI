//! When silence becomes a fact — plan phase 2.
//!
//! Phase 1 refuses premature silence. That is the guardrail; it is not the
//! mechanism. Something has to decide **when** a window has closed and turn the
//! acts that stayed quiet into observations, or the sample only ever contains
//! the counterparties who replied — which is the most flattering possible
//! dataset and the least useful one.
//!
//! # A policy, and a sweep
//!
//! [`MaturityPolicy`] answers *how long do we wait for this kind of act*, and
//! [`mature_silences`] applies it. They are separate because the waiting period
//! is a judgement that changes with domain and the sweep is mechanical — mixing
//! them would make "we waited two weeks" a fact buried in a loop rather than a
//! configuration somebody chose.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use super::store::{OutcomeScope, OutcomeStore};
use super::types::{Confounder, DeliveryState, OutcomeLabel, RecordOutcome};

/// Confounder kinds §3 names explicitly.
///
/// Constants rather than an enum, because [`Confounder`] is deliberately open —
/// these are the ones the plan says will dominate a small sample, and naming
/// them stops the same idea being recorded as `warm`, `warm_intro` and
/// `introduced` in three places, which would split a cohort that should be one.
pub mod confounder_kind {
    /// Warm versus cold. §3: *"the strongest effect will usually be warm versus
    /// cold, not anything about the copy."*
    pub const INTRODUCTION: &str = "introduction";
    /// Whether a named introducer was involved.
    pub const INTRODUCER: &str = "introducer";
    /// Where the counterparty is in their own process.
    pub const STAGE: &str = "stage";
    /// Rough size of the counterparty organisation.
    pub const ORG_SIZE: &str = "org_size";
}

/// How long to wait before silence counts, by variant.
///
/// A window is a judgement about the domain: an accelerator that has not replied
/// in two weeks has decided, a support ticket that has not replied in two hours
/// has not. Getting it wrong in one direction records decisions that were never
/// made; in the other, the sample never fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaturityPolicy {
    default_window: Duration,
    by_variant: HashMap<String, Duration>,
}

impl MaturityPolicy {
    /// A policy with one window for everything.
    ///
    /// Refuses a non-positive window: a window of zero makes silence mature the
    /// instant an act is sent, which records "they did not reply" about someone
    /// who has not had the chance. That is the failure the whole phase exists to
    /// prevent, so it cannot be reached by configuration.
    pub fn new(default_window: Duration) -> Result<Self> {
        if default_window <= Duration::zero() {
            anyhow::bail!(
                "a maturity window must be positive: zero would mature silence the instant an \
                 act is sent, recording a decision nobody had the chance to make"
            );
        }
        Ok(Self {
            default_window,
            by_variant: HashMap::new(),
        })
    }

    /// Override the window for one variant.
    pub fn with_variant_window(
        mut self,
        variant_ref: impl Into<String>,
        window: Duration,
    ) -> Result<Self> {
        if window <= Duration::zero() {
            anyhow::bail!("a maturity window must be positive");
        }
        self.by_variant.insert(variant_ref.into(), window);
        Ok(self)
    }

    pub fn window_for(&self, variant_ref: &str) -> Duration {
        self.by_variant
            .get(variant_ref)
            .copied()
            .unwrap_or(self.default_window)
    }

    /// When an act sent at `acted_at` stops being "not yet".
    pub fn matures_at(&self, variant_ref: &str, acted_at: DateTime<Utc>) -> DateTime<Utc> {
        acted_at + self.window_for(variant_ref)
    }
}

/// An act that has been performed and is waiting to see whether anything comes
/// back.
///
/// Supplied by the caller rather than discovered here: what counts as an
/// outstanding act depends on the subsystem that performed it, and a sweep that
/// went looking would need to know about all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitingOutcome {
    pub act_ref: String,
    pub variant_ref: String,
    pub variant_version: String,
    pub engagement_id: Option<String>,
    pub program_id: Option<String>,
    pub acted_at: DateTime<Utc>,
    pub delivery_state: DeliveryState,
    pub confounders: Vec<Confounder>,
}

/// Why an act was not matured, so a caller can tell "too early" from "already
/// answered" rather than seeing one silent skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotMatured {
    /// The window has not closed.
    StillOpen { matures_at: DateTime<Utc> },
    /// Something came back, so this was never silence.
    ///
    /// Any recorded outcome counts, not only an engagement. A **rejection** is
    /// the case that makes the distinction matter: it is not engagement, but the
    /// counterparty answered, and maturing it into silence would record that
    /// they both replied and ignored us.
    AlreadyAnswered { label: OutcomeLabel },
    /// Silence was already recorded for it.
    AlreadyRecorded,
}

/// What one sweep did.
#[derive(Debug, Clone, Default)]
pub struct MaturationReport {
    pub matured: Vec<String>,
    pub skipped: Vec<(String, NotMatured)>,
}

impl MaturationReport {
    pub fn matured_count(&self) -> usize {
        self.matured.len()
    }
}

/// Record `Silent` for every awaiting act whose window has closed and which
/// nothing came back on.
///
/// # The check that matters
///
/// An act the counterparty **answered** must never mature into silence. The
/// sweep therefore reads the act's existing observations rather than trusting
/// the caller's list to be current: a caller assembling "still awaiting" from
/// its own state is exactly the thing that goes stale, and the cost of it being
/// stale here is a recorded claim that somebody ignored us when they did not.
///
/// Idempotent through the store, and additionally short-circuited here so a
/// repeated sweep neither re-derives nor re-records a decision it already made.
///
/// The short-circuit is consulted BEFORE ripeness, deliberately: a settled act
/// must read as settled no matter what the window says today, or widening the
/// window in configuration makes an already-concluded act report as still open.
pub fn mature_silences(
    store: &OutcomeStore,
    scope: &OutcomeScope,
    awaiting: &[AwaitingOutcome],
    policy: &MaturityPolicy,
    now: DateTime<Utc>,
) -> Result<MaturationReport> {
    let mut report = MaturationReport::default();

    for act in awaiting {
        // WHAT IS ALREADY SETTLED IS ANSWERED BEFORE WHAT IS RIPE, and the order
        // matters more than it looks. Deriving ripeness first meant a settled act
        // reported `StillOpen { matures_at }` the moment somebody widened the
        // window in configuration: the act concluded silent on the 15th would
        // read as "still open, matures on the 29th", and a caller acting on that
        // report would chase a counterparty whose silence is already a recorded
        // fact. Nothing was ever re-recorded — the safety held — but the answer
        // was wrong, which is its own failure. Found by test, not by reading.
        //
        // The cost is one store read per act per sweep rather than per RIPE act.
        // Unripe acts are recent by definition, so the set is bounded by the
        // window, and a per-act log is a small read.
        let existing = store.observations_for_act(scope, &act.act_ref)?;
        if existing
            .iter()
            .any(|observation| observation.label == OutcomeLabel::Silent)
        {
            report
                .skipped
                .push((act.act_ref.clone(), NotMatured::AlreadyRecorded));
            continue;
        }
        // ANY recorded outcome means something came back — not only an
        // engagement. A rejection is the case that makes this matter: it is not
        // engagement, but the counterparty answered, and maturing it into
        // silence would record that they both replied and ignored us.
        if let Some(answered) = existing.first() {
            report.skipped.push((
                act.act_ref.clone(),
                NotMatured::AlreadyAnswered {
                    label: answered.label,
                },
            ));
            continue;
        }

        // Nothing is recorded about this act yet, so ripeness decides.
        let matures_at = policy.matures_at(&act.variant_ref, act.acted_at);
        if now < matures_at {
            report
                .skipped
                .push((act.act_ref.clone(), NotMatured::StillOpen { matures_at }));
            continue;
        }

        store.record(
            scope,
            &RecordOutcome {
                engagement_id: act.engagement_id.clone(),
                program_id: act.program_id.clone(),
                act_ref: act.act_ref.clone(),
                variant_ref: act.variant_ref.clone(),
                variant_version: act.variant_version.clone(),
                label: OutcomeLabel::Silent,
                // Carried through unchanged. A bounce that then went quiet is
                // still recorded — omitting it would make the cohort count
                // disagree with the number of acts performed — and
                // `usable_cohort` is what excludes it from evidence.
                delivery_state: act.delivery_state,
                matured_at: Some(matures_at),
                confounders: act.confounders.clone(),
            },
            now,
        )?;
        report.matured.push(act.act_ref.clone());
    }

    Ok(report)
}
