//! The cadence that turns recorded outcomes into decisions an owner can make.
//!
//! Doc: `docs/components/magician/outcome-learning.md`.
//!
//! [`super::sweep_candidates_into_learning`] is a composition somebody has to
//! call, and the whole point of the loop is that nobody has to remember. A
//! comparison that only happens when a person asks for it is a comparison that
//! happens once, in the week somebody built the feature — after which the
//! observations keep accumulating and nothing is ever proposed from them again.
//!
//! Cadence, gating and health follow the shape every long-lived worker in this
//! codebase uses (see [`super::super::worker`] and
//! `magician_v2::obligation_sweeps::worker`): a configured interval,
//! `MissedTickBehavior::Skip` so a slow tick does not queue a burst behind
//! itself, a [`CancellationToken`] for shutdown, and a snapshot a health
//! endpoint can read.
//!
//! # Where the tenants come from, and why not from the storage root
//!
//! From [`OutcomeMaturityConfig::scopes`] — the same explicit roster the
//! maturity sweep is given. Not discovered from the storage root, unlike the
//! obligation sweep: a proposal can only rest on recorded outcomes, the only
//! producer of those is the maturity sweep, and it sweeps exactly this roster.
//! Discovering a wider set would produce a worker that faithfully proposes
//! nothing for tenants nobody records anything about, which is indistinguishable
//! from a worker that is broken.
//!
//! # A tick that could not have proposed anything says so
//!
//! Three separate zeros are told apart, because they are three different
//! systems:
//!
//! - **no scopes at all** — `degraded`. An empty roster and an unreadable one
//!   produce the same count, and only one of them is healthy.
//! - **scopes, but no maturity sweep behind any of them** — `degraded`. Every
//!   comparison in the tick was withheld as
//!   [`SilenceNeverSwept`](super::ComparisonWithheld::SilenceNeverSwept), so the
//!   loop is running and structurally cannot say anything.
//! - **scopes, silences recorded, nothing proposed** — `idle`. That is a quiet
//!   day: the cohorts are too small yet, or every comparison is already
//!   decided. It is the ordinary case and it is not a fault.
//!
//! # It ships on, and the safety is the gate rather than the switch
//!
//! Unlike the maturity sweep, this one **chooses nothing**. It records no
//! judgement about anybody's silence, it computes no score, and every candidate
//! it files is `review_required`, medium risk, in the proposed state — the
//! substrate's auto-apply lanes open only for a low-risk candidate needing no
//! review, so *"it never applies anything; every change is an owner editorial
//! decision"* holds structurally. It also cannot produce a candidate at all
//! until a maturity sweep somebody deliberately enabled has completed a tick
//! and both cohorts clear the floor. The failure mode of not running it is the
//! one this whole programme is named after: evidence recorded and never
//! surfaced.

use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::RwLock,
    task::JoinHandle,
    time::{interval, Duration as TokioDuration},
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use magician::config::OutcomeMaturityConfig;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::learning::LearningCandidateType;

use super::super::feeders::{EvidenceFloor, LearningTarget};
use super::super::store::OutcomeScope;
use super::super::worker::MaturityWorkerHealth;
use super::{sweep_scope, CandidateSweep, ComparisonWithheld, MaturitySweepStanding};

const LOG_TARGET: &str = "outcome_learning::proposal_worker";

/// The floor on the tick interval.
///
/// A maturity window is measured in days and a cohort fills over weeks; there
/// is nothing a proposal pass can learn by running every second except how to
/// re-read a learning substrate that has not changed. Applied with `max`, so a
/// misconfigured zero becomes this rather than a spin.
const MIN_TICK_INTERVAL_SECS: u64 = 60;

/// What a proposal pass may say, and how often it looks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutcomeProposalConfig {
    pub enabled: bool,
    /// Durable operator pause. When true, no cohort is read and no candidate is
    /// filed.
    pub paused: bool,
    /// How often to look, in seconds. Floored at 60 by the worker.
    pub tick_interval_secs: u64,
    /// The smallest usable sample either cohort may carry.
    ///
    /// Refused below the module's own minimum rather than substituted — and in
    /// particular it cannot be zero, which an empty cohort satisfies vacuously.
    pub minimum_usable: usize,
    /// The smallest number of distinct counterparties either cohort may rest
    /// on. One counterparty is a fact about that counterparty, not about the
    /// variant.
    pub minimum_counterparties: usize,
    /// Where a converted comparison lands in the learning substrate.
    ///
    /// Configurable because the same comparison can propose a change to a
    /// template, a persona or a procedure, and hard-coding one would tie this
    /// loop to a single flow.
    pub candidate_type: LearningCandidateType,
}

impl Default for OutcomeProposalConfig {
    /// On, hourly, at the module's own floors.
    ///
    /// See the module note for why on: this pass decides nothing and applies
    /// nothing, and it is inert until a maturity sweep an operator enabled has
    /// recorded something.
    ///
    /// [`LearningCandidateType::WorkflowTemplate`] because a variant of an
    /// outward act is a template of how a piece of work is done. `Other` would
    /// have been the timid choice and the worse one: every proposal would land
    /// in the bucket nobody filters on.
    fn default() -> Self {
        Self {
            enabled: true,
            paused: false,
            tick_interval_secs: 3600,
            minimum_usable: super::super::proposal::MINIMUM_COHORT,
            minimum_counterparties: super::super::proposal::MINIMUM_COUNTERPARTIES,
            candidate_type: LearningCandidateType::WorkflowTemplate,
        }
    }
}

impl OutcomeProposalConfig {
    /// The evidence floor this configuration describes, or why it cannot hold.
    ///
    /// Every refusal is [`EvidenceFloor`]'s own, so a misconfigured file fails
    /// the worker at startup rather than producing a pass that files a
    /// comparison of nothing against nothing as a finding.
    pub fn floor(&self) -> Result<EvidenceFloor> {
        EvidenceFloor::new(self.minimum_usable, self.minimum_counterparties)
    }
}

/// What the last tick did — counts, never rates.
///
/// *"Six comparisons, one proposed, three too small, two already decided"* is a
/// sentence an operator can reconcile against the substrate in front of them. A
/// single "proposal rate" would hide which of those numbers moved and would
/// read as progress whichever direction it went.
#[derive(Debug, Clone, Default, Serialize)]
pub struct OutcomeProposalHealthSnapshot {
    pub enabled: bool,
    pub paused: bool,
    pub state: String,
    pub tick_interval_secs: u64,
    pub minimum_usable: usize,
    pub minimum_counterparties: usize,
    pub candidate_type: String,
    pub last_tick_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub scopes_seen: usize,
    pub scopes_failed: usize,
    /// Scopes whose silences a maturity sweep is actually recording. Zero here
    /// with scopes seen is the structurally-mute tick.
    pub scopes_with_silence_swept: usize,
    pub comparisons_considered: usize,
    pub proposed: usize,
    /// Comparisons refused because a cohort was too small or too narrow.
    pub withheld_under_floor: usize,
    /// Comparisons refused because no silence is being recorded for the scope.
    pub withheld_silence_never_swept: usize,
    /// Comparisons whose candidate already exists. This is the number a second
    /// tick reports where the first reported `proposed`.
    pub already_proposed: usize,
}

impl OutcomeProposalHealthSnapshot {
    pub(crate) fn configured(config: &OutcomeProposalConfig) -> Self {
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
            minimum_usable: config.minimum_usable,
            minimum_counterparties: config.minimum_counterparties,
            candidate_type: config.candidate_type.as_str().to_string(),
            ..Self::default()
        }
    }

    fn begin_tick(&mut self, at: &str) {
        self.state = "running".to_string();
        self.last_tick_at = Some(at.to_string());
        self.last_error = None;
        self.scopes_seen = 0;
        self.scopes_failed = 0;
        self.scopes_with_silence_swept = 0;
        self.comparisons_considered = 0;
        self.proposed = 0;
        self.withheld_under_floor = 0;
        self.withheld_silence_never_swept = 0;
        self.already_proposed = 0;
    }

    fn absorb(&mut self, sweep: &CandidateSweep) {
        self.comparisons_considered += sweep.considered;
        self.proposed += sweep.proposed.len();
        for withheld in &sweep.withheld {
            match withheld.reason {
                ComparisonWithheld::NotProposable(_) => self.withheld_under_floor += 1,
                ComparisonWithheld::AlreadyProposed { .. } => self.already_proposed += 1,
                ComparisonWithheld::SilenceNeverSwept { .. } => {
                    self.withheld_silence_never_swept += 1
                },
            }
        }
    }
}

/// Settle a finished tick's state.
///
/// Split out and tested directly, because the interesting decision is which
/// zero means what — see the module note on the three of them.
pub(crate) fn finalize_tick(snapshot: &mut OutcomeProposalHealthSnapshot, completed_at: &str) {
    if snapshot.scopes_seen == 0 {
        snapshot.state = "degraded".to_string();
        snapshot.last_error = Some(
            "no scope was named to the proposal pass, so nothing was compared; a roster nobody \
             filled in and a roster that could not be read produce the same count"
                .to_string(),
        );
        return;
    }
    if snapshot.scopes_failed > 0 {
        snapshot.state = "degraded".to_string();
        return;
    }
    if snapshot.scopes_with_silence_swept == 0 {
        snapshot.state = "degraded".to_string();
        snapshot.last_error = Some(
            "no scope in this pass has its silences recorded by a maturity sweep, so every \
             cohort holds only the counterparties who answered and every comparison was \
             withheld; the loop is running and structurally cannot say anything"
                .to_string(),
        );
        return;
    }
    snapshot.state = "idle".to_string();
    snapshot.last_success_at = Some(completed_at.to_string());
}

#[derive(Clone)]
pub struct OutcomeProposalHealth {
    snapshot: Arc<RwLock<OutcomeProposalHealthSnapshot>>,
}

impl OutcomeProposalHealth {
    pub fn new(config: &OutcomeProposalConfig) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(OutcomeProposalHealthSnapshot::configured(
                config,
            ))),
        }
    }

    pub async fn snapshot(&self) -> OutcomeProposalHealthSnapshot {
        self.snapshot.read().await.clone()
    }

    fn mark_degraded(&self, message: impl Into<String>) {
        if let Ok(mut snapshot) = self.snapshot.try_write() {
            snapshot.state = "degraded".to_string();
            snapshot.last_error = Some(message.into());
        }
    }
}

/// The periodic proposal pass. **The named entry point** for
/// [`candidates_to_learning`](super::super::feeders::candidates_to_learning).
pub struct OutcomeProposalWorker {
    handle: Option<JoinHandle<()>>,
    cancel: CancellationToken,
    health: OutcomeProposalHealth,
}

impl OutcomeProposalWorker {
    /// Start the pass, or decline to and say why.
    ///
    /// # The two configurations it reads, and why both
    ///
    /// `config` is its own. `maturity` is the maturity sweep's, and it is read
    /// for exactly two facts: which tenants have their silences recorded, and
    /// whether that sweep is switched on at all. Those are what
    /// [`MaturitySweepStanding`] carries, and without them a cohort holding only
    /// the counterparties who answered would be summarised as if it held
    /// everyone.
    ///
    /// `maturity_health` supplies the third fact — whether a tick has actually
    /// completed. Configuration alone would report a sweep that was enabled and
    /// then declined its own window as covering the scope. Passing `None` is
    /// honest and means *"no sweep is running"*; it is never the same as passing
    /// a health handle that has never succeeded, and both withhold.
    ///
    /// Declines — visibly, through the health snapshot — when the config is off
    /// or paused, and when the configured floor cannot be honoured. A pass that
    /// started anyway with a substituted floor would file comparisons that
    /// cannot support themselves into an append-only decision log.
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        config: OutcomeProposalConfig,
        maturity: OutcomeMaturityConfig,
        maturity_health: Option<MaturityWorkerHealth>,
        cancel: CancellationToken,
    ) -> Self {
        let health = OutcomeProposalHealth::new(&config);
        if !config.enabled || config.paused {
            info!(
                target: LOG_TARGET,
                enabled = config.enabled,
                paused = config.paused,
                "outcome proposal pass not started by configuration"
            );
            return Self {
                handle: None,
                cancel,
                health,
            };
        }

        let floor = match config.floor() {
            Ok(floor) => floor,
            Err(error) => {
                health.mark_degraded(format!(
                    "the configured evidence floor cannot hold, so nothing was started: {error}"
                ));
                warn!(target: LOG_TARGET, %error, "outcome proposal pass enabled with an unusable floor");
                return Self {
                    handle: None,
                    cancel,
                    health,
                };
            },
        };

        // The roster, resolved once: it is configuration, and re-reading it per
        // tick would only matter if the file could change under a running
        // process, which it cannot.
        let scopes: Vec<(String, String)> = maturity
            .scopes
            .iter()
            .map(|declared| {
                (
                    declared.principal.trim().to_string(),
                    declared.workspace.trim().to_string(),
                )
            })
            .filter(|(principal, workspace)| !principal.is_empty() && !workspace.is_empty())
            .collect();
        // A sweep that is off records nothing, whatever its roster says.
        let maturity_running = maturity.enabled && !maturity.paused;
        let target = LearningTarget::new(config.candidate_type.clone());

        let worker_cancel = cancel.clone();
        let worker_health = health.clone();
        let handle = tokio::spawn(async move {
            let cadence =
                TokioDuration::from_secs(config.tick_interval_secs.max(MIN_TICK_INTERVAL_SECS));
            let mut ticker = interval(cadence);
            // A slow tick must not queue a burst behind itself: the pass is
            // idempotent at the substrate, so a skipped tick costs a delay and a
            // stampede of catch-up ticks costs a filesystem.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = worker_cancel.cancelled() => break,
                    _ = ticker.tick() => {},
                }

                {
                    let mut snapshot = worker_health.snapshot.write().await;
                    snapshot.begin_tick(&Utc::now().to_rfc3339());
                }

                // Re-read every tick: a maturity sweep that has not completed a
                // tick yet completes one later, and this is how the gate opens
                // without a restart.
                let completed_a_tick = match maturity_health.as_ref() {
                    Some(health) => health.snapshot().await.last_success_at.is_some(),
                    None => false,
                };
                let standing = if maturity_running {
                    MaturitySweepStanding::new(scopes.clone(), completed_a_tick)
                } else {
                    MaturitySweepStanding::never_swept()
                };

                for (principal, workspace) in &scopes {
                    let covered = standing.covers(principal, workspace);
                    let layout = workspace_layout.clone();
                    let scope = OutcomeScope::new(principal.clone(), workspace.clone());
                    let standing = standing.clone();
                    let target = target.clone();
                    // One instant per scope, taken inside the blocking half, so
                    // a cohort cannot ripen between the two summaries.
                    let outcome = tokio::task::spawn_blocking(move || {
                        sweep_scope(&layout, &scope, floor, &target, &standing, Utc::now())
                    })
                    .await;

                    let mut snapshot = worker_health.snapshot.write().await;
                    snapshot.scopes_seen += 1;
                    if covered {
                        snapshot.scopes_with_silence_swept += 1;
                    }
                    match outcome {
                        Ok(Ok(sweep)) => snapshot.absorb(&sweep),
                        // A scope that failed is counted and the tick
                        // continues: one tenant's unreadable substrate must not
                        // stop every other tenant's comparisons. The tick still
                        // ends degraded, so a scope that fails every time stays
                        // visible rather than being averaged away.
                        Ok(Err(error)) => {
                            snapshot.scopes_failed += 1;
                            warn!(target: LOG_TARGET, %error, %principal, %workspace, "outcome proposal pass failed for one scope");
                        },
                        Err(error) => {
                            snapshot.scopes_failed += 1;
                            warn!(target: LOG_TARGET, %error, %principal, %workspace, "outcome proposal task did not complete");
                        },
                    }
                }

                let mut snapshot = worker_health.snapshot.write().await;
                finalize_tick(&mut snapshot, &Utc::now().to_rfc3339());
            }
            info!(target: LOG_TARGET, "outcome proposal pass cancelled");
        });

        Self {
            handle: Some(handle),
            cancel,
            health,
        }
    }

    pub fn health(&self) -> OutcomeProposalHealth {
        self.health.clone()
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle {
            let _ = handle.await;
        }
    }
}
