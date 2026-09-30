//! A cadence for the maturity sweep.
//!
//! Plan: `docs/plans/2026-08-07-opc-outcome-learning.md` §2, phase 2.
//!
//! [`super::sweep`] is a sweep somebody has to call. A sweep nobody calls
//! records nothing, and an outcome store that only ever receives replies is
//! indistinguishable from a world where everybody answers — which is the exact
//! failure the maturity phase exists to prevent, arrived at by omission rather
//! than by a bug. That is where this subsystem stood until the three missing
//! pieces landed: [`super::book::WorkspaceMaturityBook`] implements
//! [`MaturityBook`], [`magician::config::OutcomeMaturityConfig`] deserialises into
//! [`MaturitySweepConfig`], and
//! [`super::book::spawn_configured_maturity_sweep`] is the one function a boot
//! path calls.
//!
//! **The sweep still ships off.** It writes observations that later become
//! evidence and an append-only store cannot un-write them, so a process that
//! failed to state its window and its scopes records nothing rather than
//! recording silences against a waiting period nobody chose.
//!
//! Cadence, gating and health follow the shape every long-lived worker in this
//! codebase already uses (see `magician_v2::social::worker`): a configured
//! interval, `MissedTickBehavior::Skip` so a slow tick does not queue a burst
//! behind itself, a [`CancellationToken`] for shutdown, and a snapshot a health
//! endpoint can read. Nothing here invents scheduling, and nothing here
//! enumerates anybody's storage — [`super::book`] does both of those.
//!
//! # Where the work comes from, and why it is a trait
//!
//! A sweep needs three things a timer cannot know: which tenants to sweep,
//! which acts are outstanding, and which variant version each was performed
//! under. The last is the deciding subsystem's fact — the act itself does not
//! record it — so this module takes a [`MaturityBook`] rather than reaching
//! into anybody's storage.
//!
//! That is the same refusal `super::feeders` makes and for the same reason: a
//! worker that knew how to enumerate one subsystem's outward acts would only
//! ever mature that subsystem's silences.
//!
//! # An empty book is reported, never celebrated
//!
//! A tick that swept no scopes reports `degraded`, not `idle`. *"We found
//! nothing to do"* and *"nobody told us what to do"* produce the same zero, and
//! only one of them is healthy — treating the vacuous case as success is how a
//! worker runs for a year having done nothing while every dashboard reads
//! green.
//!
//! The same rule one level in: a tick that found outward acts and could bind a
//! cohort key to **none** of them is `degraded` too. An empty binding list and
//! four hundred acts nobody has declared a variant for produce the same zero at
//! the sweep, and the second is a subsystem that will keep reporting quiet days
//! for as long as nobody looks.
//!
//! # A failing scope does not stop the others
//!
//! Each scope is swept independently and a failure is counted and logged rather
//! than aborting the tick. One tenant's unreadable log must not stop every
//! other tenant's silences from maturing — but the tick still reports
//! `degraded`, so a scope that fails every time is visible rather than averaged
//! away.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tokio::{
    sync::RwLock,
    task::JoinHandle,
    time::{interval, Duration as TokioDuration},
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::evidence::outward_assertions::{OutwardAssertionStore, OutwardScope};

use super::maturity::MaturityPolicy;
use super::store::{OutcomeScope, OutcomeStore};
use super::sweep::{run_maturity_sweep, ActCohortBinding, MaturitySweepReport};

const LOG_TARGET: &str = "outcome_learning::maturity_worker";

/// The floor on the tick interval.
///
/// A maturity window is measured in days; there is nothing a sweep can learn by
/// running every second except how to hammer a filesystem. The floor is applied
/// with `max`, so a misconfigured zero becomes this rather than a spin.
const MIN_TICK_INTERVAL_SECS: u64 = 60;

/// One tenant's slice of the sweep.
///
/// The bindings are **supplied**, never discovered here. Which acts are
/// outstanding and which variant version each was performed under are facts the
/// subsystem that acted holds; a worker that went looking would have to know
/// about all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaturityScope {
    pub principal: String,
    pub workspace: String,
    pub bindings: Vec<ActCohortBinding>,
    /// Acts the book found and could **not** describe a cohort for, so they were
    /// not swept.
    ///
    /// Carried rather than dropped because *"this scope has nothing outstanding"*
    /// and *"this scope has four hundred outstanding acts and nobody has said
    /// which variant any of them is"* produce the same empty binding list, and
    /// only one of them is a quiet day. [`finalize_tick`] reads it.
    pub acts_unbound: usize,
    /// Acts in the scope that **no kind of work names at all**, so no work axis
    /// could reach them.
    ///
    /// An outward act carries `program_id` and `engagement_id` and no other work
    /// field, so an act bound to an account, a panel or a person — or to no work
    /// — is invisible to a work-axis enumeration however wide the axis list
    /// gets. Counted so that gap is a number somebody can read rather than a
    /// sweep that silently considered less than it was asked to.
    pub acts_without_work: usize,
}

/// Where a tick's work comes from.
///
/// Implemented by whoever knows which acts are outstanding. Returning an empty
/// list is legitimate — a quiet day — and is reported as such rather than
/// treated as success; returning `Err` is a book that could not be read, which
/// is never permission to conclude there was nothing to mature.
#[async_trait]
pub trait MaturityBook: Send + Sync {
    /// Every scope this tick should sweep, with the acts to consider in each.
    async fn scopes(&self) -> Result<Vec<MaturityScope>>;
}

/// How long to wait before silence counts, as configuration.
///
/// Days rather than a [`Duration`], because this is what an operator writes
/// down and a window is a judgement about a domain rather than a tuning knob.
/// [`MaturitySweepConfig::policy`] refuses a non-positive value: a zero window
/// matures silence the instant an act is sent, recording a decision nobody had
/// the chance to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaturitySweepConfig {
    pub enabled: bool,
    pub paused: bool,
    pub tick_interval_secs: u64,
    /// The default waiting window, in days.
    pub default_window_days: i64,
    /// Per-variant overrides, in days.
    pub variant_window_days: Vec<(String, i64)>,
}

impl Default for MaturitySweepConfig {
    /// Off, with a two-week window and an hourly cadence.
    ///
    /// **Disabled by default and never the other way.** This worker writes
    /// observations that later become evidence, and an unconfigured process
    /// that started recording silences would fill a cohort with a window nobody
    /// chose. Fourteen days is the plan's own example — *"an accelerator that
    /// has not replied in two weeks has decided"* — and it is a starting point
    /// an operator is expected to set, not a default that quietly applies.
    fn default() -> Self {
        Self {
            enabled: false,
            paused: false,
            tick_interval_secs: 3600,
            default_window_days: 14,
            variant_window_days: Vec::new(),
        }
    }
}

impl MaturitySweepConfig {
    /// Build the policy this configuration describes, or refuse.
    ///
    /// Every refusal is [`MaturityPolicy`]'s own: a non-positive window cannot
    /// be expressed, so a misconfigured file fails the worker at startup rather
    /// than producing a sweep that records "they did not reply" about people
    /// who have not had the chance.
    pub fn policy(&self) -> Result<MaturityPolicy> {
        let mut policy = MaturityPolicy::new(Duration::days(self.default_window_days))?;
        for (variant_ref, days) in &self.variant_window_days {
            policy = policy.with_variant_window(variant_ref.clone(), Duration::days(*days))?;
        }
        Ok(policy)
    }
}

/// What the last tick did — counts, never rates.
///
/// *"Nine acts considered, two matured, six still open, one already settled"*
/// is a sentence an operator can act on. A single "maturity rate" would hide
/// which of those numbers moved, and would read as progress whichever
/// direction it went.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MaturityWorkerHealthSnapshot {
    pub enabled: bool,
    pub paused: bool,
    pub state: String,
    pub tick_interval_secs: u64,
    pub default_window_days: i64,
    pub last_tick_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub scopes_seen: usize,
    pub scopes_failed: usize,
    pub acts_considered: usize,
    pub matured: usize,
    pub still_open: usize,
    pub already_settled: usize,
    pub not_collected: usize,
    pub not_awaiting: usize,
    /// Acts the book found that no cohort declaration describes.
    pub acts_unbound: usize,
    /// Acts no kind of work names, so no work axis could reach them.
    pub acts_without_work: usize,
}

impl MaturityWorkerHealthSnapshot {
    fn configured(config: &MaturitySweepConfig) -> Self {
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
            default_window_days: config.default_window_days,
            ..Self::default()
        }
    }

    fn begin_tick(&mut self, at: &DateTime<Utc>) {
        self.state = "running".to_string();
        self.last_tick_at = Some(at.to_rfc3339());
        self.last_error = None;
        self.scopes_seen = 0;
        self.scopes_failed = 0;
        self.acts_considered = 0;
        self.matured = 0;
        self.still_open = 0;
        self.already_settled = 0;
        self.not_collected = 0;
        self.not_awaiting = 0;
        self.acts_unbound = 0;
        self.acts_without_work = 0;
    }

    fn absorb(&mut self, report: &MaturitySweepReport) {
        self.acts_considered += report.considered;
        self.matured += report.matured_count();
        self.still_open += report.still_open_count();
        self.already_settled += report.already_settled_count();
        self.not_collected += report.not_collected.len();
        self.not_awaiting += report.not_awaiting.len();
    }
}

/// Settle a finished tick's state.
///
/// Split out and tested directly, because the interesting decision is which
/// zero means what. A tick that swept **no scopes at all** is `degraded`: an
/// empty book and a book nobody supplied produce the same counts, and only one
/// of those is a healthy system. A tick that swept scopes and matured nothing
/// is `idle` — that is a quiet day, which is the ordinary case.
///
/// The third rule is the same idea one level in: a tick whose book found
/// outward acts and could bind a cohort to **none** of them considered nothing,
/// so every count below it is zero and reads as that same quiet day.
fn finalize_tick(snapshot: &mut MaturityWorkerHealthSnapshot, completed_at: &DateTime<Utc>) {
    if snapshot.scopes_seen == 0 {
        snapshot.state = "degraded".to_string();
        snapshot.last_error = Some(
            "the maturity book named no scopes, so nothing was swept; an empty book and an \
             unreadable one produce the same counts"
                .to_string(),
        );
        return;
    }
    if snapshot.scopes_failed > 0 {
        snapshot.state = "degraded".to_string();
        return;
    }
    // Acts were found and not one of them could be described. The sweep saw
    // nothing, so every count below it is zero — which is indistinguishable
    // from a quiet day unless this says otherwise. A subsystem reporting `idle`
    // in that state is the exact shape of the failure this whole phase is
    // climbing out of, one level further in.
    if snapshot.acts_considered == 0 && snapshot.acts_unbound > 0 {
        snapshot.state = "degraded".to_string();
        snapshot.last_error = Some(format!(
            "{} outward acts were found and none of them carries a declared cohort, so nothing \
             was swept; an act with no variant version is comparable to nothing, and binding one \
             to a default would record an observation that cannot be withdrawn",
            snapshot.acts_unbound
        ));
        return;
    }
    snapshot.state = "idle".to_string();
    snapshot.last_success_at = Some(completed_at.to_rfc3339());
}

#[derive(Clone)]
pub struct MaturityWorkerHealth {
    snapshot: Arc<RwLock<MaturityWorkerHealthSnapshot>>,
}

impl MaturityWorkerHealth {
    pub fn new(config: &MaturitySweepConfig) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(MaturityWorkerHealthSnapshot::configured(
                config,
            ))),
        }
    }

    pub async fn snapshot(&self) -> MaturityWorkerHealthSnapshot {
        self.snapshot.read().await.clone()
    }

    fn mark_degraded(&self, message: impl Into<String>) {
        if let Ok(mut snapshot) = self.snapshot.try_write() {
            snapshot.state = "degraded".to_string();
            snapshot.last_error = Some(message.into());
        }
    }
}

/// The periodic maturity sweep.
pub struct MaturityWorker {
    handle: Option<JoinHandle<()>>,
    cancel: CancellationToken,
    health: MaturityWorkerHealth,
}

impl MaturityWorker {
    /// Start the worker, or decline to and say why.
    ///
    /// Declines — visibly, through the health snapshot — when the config is off
    /// or paused, and when the configured window cannot be honoured. A worker
    /// that started anyway with a substituted window would record silences
    /// against a waiting period nobody chose, and those observations cannot be
    /// un-recorded: the store is append-only by design, because *"the data
    /// cannot be reconstructed afterwards"* cuts both ways.
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        book: Arc<dyn MaturityBook>,
        config: MaturitySweepConfig,
        cancel: CancellationToken,
    ) -> Self {
        let health = MaturityWorkerHealth::new(&config);
        if !config.enabled || config.paused {
            info!(
                target: LOG_TARGET,
                enabled = config.enabled,
                paused = config.paused,
                "maturity sweep not started by configuration"
            );
            return Self {
                handle: None,
                cancel,
                health,
            };
        }

        let policy = match config.policy() {
            Ok(policy) => policy,
            Err(error) => {
                health.mark_degraded(format!(
                    "the configured maturity window cannot be honoured, so nothing was started: \
                     {error}"
                ));
                warn!(target: LOG_TARGET, %error, "maturity sweep enabled with an unusable window");
                return Self {
                    handle: None,
                    cancel,
                    health,
                };
            },
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

                let scopes = match book.scopes().await {
                    Ok(scopes) => scopes,
                    Err(error) => {
                        let mut snapshot = worker_health.snapshot.write().await;
                        snapshot.state = "degraded".to_string();
                        // An unreadable book is NOT an empty one. Reporting it
                        // as a quiet tick would present "we could not see the
                        // work" as "there was no work".
                        snapshot.last_error = Some(
                            "the maturity book could not be read; nothing was swept this tick"
                                .to_string(),
                        );
                        warn!(target: LOG_TARGET, %error, "maturity book unreadable");
                        continue;
                    },
                };

                for scope in scopes {
                    let layout = workspace_layout.clone();
                    let policy = policy.clone();
                    // Read off the scope before it moves into the blocking half.
                    // These two are the book's own facts about what it could NOT
                    // hand over, and they survive a scope whose sweep then fails
                    // — which is the case where they matter most.
                    let acts_unbound = scope.acts_unbound;
                    let acts_without_work = scope.acts_without_work;
                    let outcome =
                        tokio::task::spawn_blocking(move || sweep_one(&layout, &scope, &policy))
                            .await;
                    let mut snapshot = worker_health.snapshot.write().await;
                    snapshot.scopes_seen += 1;
                    snapshot.acts_unbound += acts_unbound;
                    snapshot.acts_without_work += acts_without_work;
                    match outcome {
                        Ok(Ok(report)) => snapshot.absorb(&report),
                        // A scope that failed is counted and the tick continues:
                        // one tenant's unreadable log must not stop every other
                        // tenant's silences from maturing. The tick still ends
                        // degraded, so a scope that fails every time stays
                        // visible rather than being averaged away.
                        Ok(Err(error)) => {
                            snapshot.scopes_failed += 1;
                            warn!(target: LOG_TARGET, %error, "maturity sweep failed for one scope");
                        },
                        Err(error) => {
                            snapshot.scopes_failed += 1;
                            warn!(target: LOG_TARGET, %error, "maturity sweep task did not complete");
                        },
                    }
                }

                let mut snapshot = worker_health.snapshot.write().await;
                finalize_tick(&mut snapshot, &Utc::now());
            }
            info!(target: LOG_TARGET, "maturity sweep cancelled");
        });

        Self {
            handle: Some(handle),
            cancel,
            health,
        }
    }

    /// A worker configuration deliberately turned off never started.
    ///
    /// Not `degraded`: the health snapshot reports `disabled` or `paused`,
    /// which is what an operator who set the switch expects to read. Kept apart
    /// from [`Self::declined`] because *"we chose not to run this"* and *"this
    /// could not be started"* must not present the same way — collapsing them
    /// would either alarm somebody about a switch they set, or hide a
    /// misconfiguration behind one.
    pub fn not_started(config: &MaturitySweepConfig, cancel: CancellationToken) -> Self {
        Self {
            handle: None,
            cancel,
            health: MaturityWorkerHealth::new(config),
        }
    }

    /// A worker that was **enabled and could not be started**, and says why.
    ///
    /// Degraded from birth. The alternative — returning an error to a boot path
    /// — ends either in taking a process down over a sweep or, far more likely,
    /// in a `let _ = …` that discards the failure entirely.
    pub fn declined(
        config: &MaturitySweepConfig,
        cancel: CancellationToken,
        reason: impl Into<String>,
    ) -> Self {
        let health = MaturityWorkerHealth::new(config);
        health.mark_degraded(reason);
        Self {
            handle: None,
            cancel,
            health,
        }
    }

    pub fn health(&self) -> MaturityWorkerHealth {
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
/// Both stores are built from the same workspace layout and the two scopes are
/// built from the same principal and workspace, so the tenant check inside
/// [`run_maturity_sweep`] can never fire from here. It is still worth having:
/// this is one caller of that function, and the check exists for the ones that
/// build their scopes separately.
fn sweep_one(
    workspace_layout: &ArtifactV2Workspace,
    scope: &MaturityScope,
    policy: &MaturityPolicy,
) -> Result<MaturitySweepReport> {
    let outward = OutwardAssertionStore::new(workspace_layout.clone());
    let outcomes = OutcomeStore::new(workspace_layout.clone());
    run_maturity_sweep(
        &outward,
        &OutwardScope::new(scope.principal.clone(), scope.workspace.clone()),
        &outcomes,
        &OutcomeScope::new(scope.principal.clone(), scope.workspace.clone()),
        &scope.bindings,
        policy,
        Utc::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker that swept nothing must not report success.
    ///
    /// An empty book and a book nobody wired produce identical counts, and the
    /// second is the state this whole phase exists to escape — the sweep is
    /// running, every number is zero, and no silence has ever matured. Reading
    /// that as `idle` is how a subsystem stays broken for a year behind a green
    /// dashboard.
    #[test]
    fn a_tick_that_swept_no_scopes_is_degraded_not_idle() {
        let now = Utc::now();
        let mut empty = MaturityWorkerHealthSnapshot::configured(&MaturitySweepConfig::default());
        finalize_tick(&mut empty, &now);
        assert_eq!(empty.state, "degraded");
        assert!(empty.last_error.is_some());
        assert!(
            empty.last_success_at.is_none(),
            "a tick that swept nothing must not stamp a success time"
        );
    }

    /// A quiet day is not a failure: scopes were swept and nothing had ripened.
    ///
    /// The distinction from the test above is the whole point — one zero means
    /// "nobody told us what to do" and the other means "we looked, and it is
    /// not time yet".
    #[test]
    fn a_tick_that_swept_scopes_and_matured_nothing_is_idle() {
        let now = Utc::now();
        let mut quiet = MaturityWorkerHealthSnapshot::configured(&MaturitySweepConfig::default());
        quiet.scopes_seen = 3;
        quiet.still_open = 9;
        finalize_tick(&mut quiet, &now);
        assert_eq!(quiet.state, "idle");
        assert_eq!(quiet.last_success_at, Some(now.to_rfc3339()));
        assert_eq!(quiet.matured, 0);
    }

    /// A scope that failed keeps the tick degraded even though other scopes
    /// succeeded, so a tenant failing every time stays visible.
    #[test]
    fn one_failed_scope_degrades_a_tick_that_otherwise_worked() {
        let now = Utc::now();
        let mut partial = MaturityWorkerHealthSnapshot::configured(&MaturitySweepConfig::default());
        partial.scopes_seen = 4;
        partial.scopes_failed = 1;
        partial.matured = 6;
        finalize_tick(&mut partial, &now);
        assert_eq!(partial.state, "degraded");
        assert!(
            partial.last_success_at.is_none(),
            "a degraded tick must not stamp a success time"
        );
    }

    /// A window of zero matures silence the instant an act is sent. The config
    /// must refuse it rather than substituting a default, because the
    /// observations it would record cannot be un-recorded.
    #[test]
    fn a_non_positive_window_is_refused_at_configuration() {
        let zero = MaturitySweepConfig {
            enabled: true,
            default_window_days: 0,
            ..MaturitySweepConfig::default()
        };
        assert!(zero.policy().is_err());

        let negative = MaturitySweepConfig {
            enabled: true,
            default_window_days: -1,
            ..MaturitySweepConfig::default()
        };
        assert!(negative.policy().is_err());

        let per_variant = MaturitySweepConfig {
            enabled: true,
            default_window_days: 14,
            variant_window_days: vec![("opening-line".to_string(), 0)],
            ..MaturitySweepConfig::default()
        };
        assert!(
            per_variant.policy().is_err(),
            "an override of zero is the same bug as a default of zero"
        );
    }

    /// The configured windows must survive into the policy, or an operator's
    /// judgement is silently replaced by the default.
    #[test]
    fn the_configured_windows_reach_the_policy() {
        let config = MaturitySweepConfig {
            enabled: true,
            default_window_days: 14,
            variant_window_days: vec![("support-reply".to_string(), 2)],
            ..MaturitySweepConfig::default()
        };
        let policy = config.policy().expect("a usable policy");
        assert_eq!(policy.window_for("support-reply"), Duration::days(2));
        assert_eq!(policy.window_for("anything-else"), Duration::days(14));
    }

    /// Nothing starts unless somebody turned it on.
    ///
    /// This worker writes observations that become evidence. A process that
    /// failed to wire its configuration must record nothing rather than record
    /// silences against a window nobody chose.
    #[test]
    fn the_sweep_is_off_until_it_is_configured_on() {
        let default = MaturitySweepConfig::default();
        assert!(!default.enabled);
        let snapshot = MaturityWorkerHealthSnapshot::configured(&default);
        assert_eq!(snapshot.state, "disabled");
    }

    /// A tick that found acts and could describe none of them is degraded.
    ///
    /// Pins the zero that is easiest to misread. When no act carries a declared
    /// cohort the sweep considers nothing, so `matured`, `still_open` and
    /// `already_settled` are all zero — exactly what a genuinely quiet day
    /// looks like. Reading that as `idle` would let a scope with four hundred
    /// outstanding acts and no variant declarations report green for ever,
    /// which is the same failure as the empty book one level further in.
    #[test]
    fn a_tick_that_bound_no_cohort_to_any_act_it_found_is_degraded() {
        let now = Utc::now();
        let mut blind = MaturityWorkerHealthSnapshot::configured(&MaturitySweepConfig::default());
        blind.scopes_seen = 2;
        blind.acts_unbound = 400;
        finalize_tick(&mut blind, &now);
        assert_eq!(blind.state, "degraded");
        assert_eq!(blind.acts_considered, 0);
        assert!(
            blind
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("400"),
            "the error must say how many acts were found, or the count is unreadable"
        );
        assert!(
            blind.last_success_at.is_none(),
            "a tick that described nothing it found must not stamp a success time"
        );
    }

    /// Some acts bound and some not is a working sweep with a visible gap.
    ///
    /// The counterpart to the test above, and the reason the rule is
    /// `considered == 0 && unbound > 0` rather than `unbound > 0`: a scope that
    /// swept nine acts and could not describe one more is doing its job, and
    /// degrading it would train an operator to ignore the state.
    #[test]
    fn a_partly_bound_tick_stays_idle_and_still_reports_the_gap() {
        let now = Utc::now();
        let mut partial = MaturityWorkerHealthSnapshot::configured(&MaturitySweepConfig::default());
        partial.scopes_seen = 1;
        partial.acts_considered = 9;
        partial.matured = 2;
        partial.acts_unbound = 1;
        partial.acts_without_work = 3;
        finalize_tick(&mut partial, &now);
        assert_eq!(partial.state, "idle");
        assert_eq!(partial.last_success_at, Some(now.to_rfc3339()));
        assert_eq!(partial.acts_unbound, 1);
        assert_eq!(partial.acts_without_work, 3);
    }

    /// A switch somebody turned off must not read as a misconfiguration.
    ///
    /// [`MaturityWorker::not_started`] and [`MaturityWorker::declined`] both
    /// return a worker with no task, and collapsing them would make an operator
    /// who set `enabled: false` see the same `degraded` as somebody whose book
    /// could not be built. One of those needs attention and the other does not.
    #[tokio::test]
    async fn a_deliberate_off_switch_and_an_unbuildable_book_do_not_report_alike() {
        let config = MaturitySweepConfig::default();
        let off = MaturityWorker::not_started(&config, CancellationToken::new()).health();
        let off = off.snapshot().await;
        assert_eq!(off.state, "disabled");
        assert_eq!(off.last_error, None);

        let enabled = MaturitySweepConfig {
            enabled: true,
            ..MaturitySweepConfig::default()
        };
        let broken =
            MaturityWorker::declined(&enabled, CancellationToken::new(), "no scopes were named")
                .health();
        let broken = broken.snapshot().await;
        assert_eq!(broken.state, "degraded");
        assert_eq!(broken.last_error.as_deref(), Some("no scopes were named"));
    }

    /// The maturity book has an implementor, and the wiring test that pinned
    /// its absence is inverted rather than deleted.
    ///
    /// It used to assert that nothing implemented [`MaturityBook`] and nothing
    /// called [`MaturityWorker::spawn`], because both were true and both
    /// headers said so — the only producer of `silent` observations had never
    /// run once, and a reviewer reading the header would have believed the
    /// survivorship bias was closed. That claim is now the other way round, and
    /// it is asserted the same way: across the whole workspace, because a unit
    /// test over `finalize_tick` cannot see a call site and the data room's own
    /// header was wrong precisely because its caller lived in a sibling crate.
    ///
    /// The spawn call site is deliberately **not** asserted here: it lives in
    /// `magician-bin/src/main.rs` beside the delivery-hygiene and obligation
    /// sweeps, and a test that pinned a line in a boot file would fail on every
    /// unrelated reordering of it.
    #[test]
    fn the_maturity_book_has_an_implementor() {
        use magician::magician_v2::doc_wiring_scan::scan_workspace;

        // `MaturityBook`'s definition lives here, and the header names it in
        // prose — which the scan skips as comment.
        const OWN_FILE: &str = "magician-learning/src/outcome_learning/worker.rs";

        let implementors = scan_workspace("MaturityBook for", &[OWN_FILE]);
        assert!(
            implementors.files_searched > 100,
            "only {} files were read, so this proves nothing",
            implementors.files_searched
        );
        assert_eq!(
            implementors.hits,
            vec!["magician-learning/src/outcome_learning/book.rs".to_string()],
            "the book that supplies this worker its work has moved or gone; without an \
             implementor no tick can ever be given anything to sweep"
        );
    }
}
