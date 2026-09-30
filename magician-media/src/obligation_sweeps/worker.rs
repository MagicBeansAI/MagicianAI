//! The cadence that fills the obligation register.
//!
//! Doc: `docs/components/magician/obligations.md`.
//!
//! [`super::sweep_scope`] is a pure composition somebody has to call, and the
//! *whole point* of the register is catching what nobody remembered. A sweep
//! that only runs when a person asks it to has the same shape as a to-do list
//! nobody opens: the promise that lapsed on Friday lapses again on Saturday and
//! nothing says so. This is the loop that runs without being asked.
//!
//! Cadence, gating and health follow the shape every long-lived worker in this
//! codebase already uses (see `magician_v2::outcome_learning::worker`): a
//! configured interval, `MissedTickBehavior::Skip` so a slow tick does not
//! queue a burst behind itself, a [`CancellationToken`] for shutdown, and a
//! snapshot a health endpoint can read.
//!
//! # Where the scopes come from
//!
//! The workspace's own `scopes/` directory, through
//! [`ArtifactV2Workspace::list_scope_segments_sync`]. Not a supplied roster:
//! the scopes ARE the tenants, the listing is the same one every other scoped
//! subsystem is addressed by, and a supplied list would make a tenant nobody
//! remembered to name invisible to the very sweep whose job is remembering.
//!
//! The relationships inside a scope are likewise discovered rather than
//! supplied — from the negotiations and the rooms the scope actually holds —
//! so no [`AudienceKind`](magician::magician_v2::audience::AudienceKind) is named
//! here, and a support account, a recruiting panel and a supplier sweep
//! through the same code.
//!
//! # An empty tick is reported, never celebrated
//!
//! A tick that swept **no scopes at all** reports `degraded`. *"There are no
//! tenants"* and *"the workspace root could not be listed"* produce the same
//! zero, and only one of them is healthy — treating the vacuous case as
//! success is exactly how a subsystem runs for a year having done nothing while
//! every dashboard reads green, which is the state this whole programme is
//! climbing out of.
//!
//! A tick that swept scopes and recorded nothing is `idle`: that is a quiet
//! day, and quiet days are the ordinary case for a register.
//!
//! # A failing scope does not stop the others
//!
//! Each scope is swept independently; a failure is counted and logged rather
//! than aborting the tick, because one tenant's unreadable log must not stop
//! every other tenant's promises from ripening. The tick still ends
//! `degraded`, so a scope that fails every time stays visible.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::RwLock,
    task::JoinHandle,
    time::{interval, Duration as TokioDuration},
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::obligations::ObligationScope;
use magician_learning::data_room::FollowUpPolicy;

use super::{sweep_scope, ScopeSweepReport, SweepMemory, SweepPolicy};

const LOG_TARGET: &str = "obligations::sweep_worker";

/// The floor on the tick interval.
///
/// Every window this sweep measures is in hours or days; there is nothing to
/// learn by running every second except how to hammer a filesystem. Applied
/// with `max`, so a misconfigured zero becomes this rather than a spin.
const MIN_TICK_INTERVAL_SECS: u64 = 60;

/// How often the register is swept, and after how long each signal ripens.
///
/// The three windows are **stated**, never inferred. They decide when an
/// owner is told somebody is late, and a window nobody chose is a cadence of
/// nagging nobody agreed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObligationSweepConfig {
    pub enabled: bool,
    pub paused: bool,
    pub tick_interval_secs: u64,
    /// How long an unanswered scheduling offer may stand before the silence
    /// ripens into a chase.
    pub silence_window_hours: i64,
    /// How long a shared, never-opened link may sit before it becomes a
    /// delivery question.
    pub delivery_question_after_hours: i64,
    /// How long an opened-but-unanswered link may sit after its last visit
    /// before it becomes a follow-up.
    pub follow_up_after_hours: i64,
}

impl Default for ObligationSweepConfig {
    /// Defaults chosen to be slower than a person would be, not faster.
    ///
    /// A sweep that chases sooner than the owner would have is worse than one
    /// that chases later: the first burns the relationship, the second only
    /// costs a day. Three days of silence on an offer, two days on a link
    /// nobody opened, five days after a read that went quiet.
    fn default() -> Self {
        Self {
            enabled: true,
            paused: false,
            tick_interval_secs: 900,
            silence_window_hours: 72,
            delivery_question_after_hours: 48,
            follow_up_after_hours: 120,
        }
    }
}

impl ObligationSweepConfig {
    /// The policy this config describes, or why it cannot be honoured.
    ///
    /// Every window is refused at or below zero rather than substituted. A
    /// zero window declares every offer silent the instant it is made and
    /// every share undelivered before the mail could arrive, and the rows it
    /// would write into an append-only register cannot be un-written.
    pub fn policy(&self) -> anyhow::Result<SweepPolicy> {
        SweepPolicy::new(
            Duration::hours(self.silence_window_hours),
            FollowUpPolicy::new(
                Duration::hours(self.delivery_question_after_hours),
                Duration::hours(self.follow_up_after_hours),
            )?,
        )
    }
}

/// What the last tick did — counts, never rates.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ObligationSweepHealthSnapshot {
    pub enabled: bool,
    pub paused: bool,
    pub state: String,
    pub tick_interval_secs: u64,
    pub silence_window_hours: i64,
    pub delivery_question_after_hours: i64,
    pub follow_up_after_hours: i64,
    pub last_tick_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub scopes_seen: usize,
    pub scopes_failed: usize,
    pub negotiations_seen: usize,
    pub rooms_seen: usize,
    pub recorded: usize,
    pub settled: usize,
    pub absent: usize,
}

impl ObligationSweepHealthSnapshot {
    pub(crate) fn configured(config: &ObligationSweepConfig) -> Self {
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
            silence_window_hours: config.silence_window_hours,
            delivery_question_after_hours: config.delivery_question_after_hours,
            follow_up_after_hours: config.follow_up_after_hours,
            ..Self::default()
        }
    }

    fn begin_tick(&mut self, at: &str) {
        self.state = "running".to_string();
        self.last_tick_at = Some(at.to_string());
        self.last_error = None;
        self.scopes_seen = 0;
        self.scopes_failed = 0;
        self.negotiations_seen = 0;
        self.rooms_seen = 0;
        self.recorded = 0;
        self.settled = 0;
        self.absent = 0;
    }

    fn absorb(&mut self, report: &ScopeSweepReport) {
        self.negotiations_seen += report.negotiations_seen;
        self.rooms_seen += report.rooms_seen;
        self.recorded += report.recorded;
        self.settled += report.settled;
        self.absent += report.absent;
    }
}

/// Settle a finished tick's state.
///
/// Split out and tested directly, because the interesting decision is which
/// zero means what. A tick that swept **no scopes at all** is `degraded`: an
/// empty workspace and an unlistable one produce the same counts, and only one
/// of those is a healthy system. A tick that swept scopes and recorded nothing
/// is `idle` — a quiet day.
pub(crate) fn finalize_tick(snapshot: &mut ObligationSweepHealthSnapshot, completed_at: &str) {
    if snapshot.scopes_seen == 0 {
        snapshot.state = "degraded".to_string();
        snapshot.last_error = Some(
            "the workspace named no scopes, so nothing was swept; a workspace with no tenants \
             and a workspace root that could not be listed produce the same counts"
                .to_string(),
        );
        return;
    }
    if snapshot.scopes_failed > 0 {
        snapshot.state = "degraded".to_string();
        return;
    }
    snapshot.state = "idle".to_string();
    snapshot.last_success_at = Some(completed_at.to_string());
}

#[derive(Clone)]
pub struct ObligationSweepHealth {
    snapshot: Arc<RwLock<ObligationSweepHealthSnapshot>>,
}

impl ObligationSweepHealth {
    pub fn new(config: &ObligationSweepConfig) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(ObligationSweepHealthSnapshot::configured(
                config,
            ))),
        }
    }

    pub async fn snapshot(&self) -> ObligationSweepHealthSnapshot {
        self.snapshot.read().await.clone()
    }

    fn mark_degraded(&self, message: impl Into<String>) {
        if let Ok(mut snapshot) = self.snapshot.try_write() {
            snapshot.state = "degraded".to_string();
            snapshot.last_error = Some(message.into());
        }
    }
}

/// The periodic obligation sweep.
pub struct ObligationSweepWorker {
    handle: Option<JoinHandle<()>>,
    cancel: CancellationToken,
    health: ObligationSweepHealth,
}

impl ObligationSweepWorker {
    /// Start the worker, or decline to and say why.
    ///
    /// Declines — visibly, through the health snapshot — when the config is off
    /// or paused, and when any configured window cannot be honoured. A worker
    /// that started anyway with a substituted window would write chases against
    /// a waiting period nobody chose, into an append-only register that cannot
    /// un-write them.
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        config: ObligationSweepConfig,
        cancel: CancellationToken,
    ) -> Self {
        let health = ObligationSweepHealth::new(&config);
        if !config.enabled || config.paused {
            info!(
                target: LOG_TARGET,
                enabled = config.enabled,
                paused = config.paused,
                "obligation sweep not started by configuration"
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
                    "the configured sweep windows cannot be honoured, so nothing was started: \
                     {error}"
                ));
                warn!(target: LOG_TARGET, %error, "obligation sweep enabled with an unusable window");
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
            // idempotent at the register, so a skipped tick costs a delay and a
            // stampede of catch-up ticks costs a filesystem.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            // The previous view, per scope, across ticks. See the module note
            // on `SweepMemory`: without it a superseded follow-up is never
            // retired. It is deliberately not persisted — a restart loses it
            // and a stale chase stays visible, which is the fail-closed
            // direction.
            let mut memories: HashMap<(String, String), SweepMemory> = HashMap::new();

            loop {
                tokio::select! {
                    _ = worker_cancel.cancelled() => break,
                    _ = ticker.tick() => {},
                }

                {
                    let mut snapshot = worker_health.snapshot.write().await;
                    snapshot.begin_tick(&Utc::now().to_rfc3339());
                }

                let scopes = match workspace_layout.list_scope_segments_sync() {
                    Ok(scopes) => scopes,
                    Err(error) => {
                        let mut snapshot = worker_health.snapshot.write().await;
                        snapshot.state = "degraded".to_string();
                        // An unlistable workspace is NOT an empty one.
                        // Reporting it as a quiet tick would present "we could
                        // not see the tenants" as "there are none".
                        snapshot.last_error = Some(
                            "the workspace scopes could not be listed; nothing was swept this \
                             tick"
                                .to_string(),
                        );
                        warn!(target: LOG_TARGET, %error, "workspace scopes unlistable");
                        continue;
                    },
                };

                for (principal, workspace) in scopes {
                    let key = (principal.clone(), workspace.clone());
                    let previous = memories.get(&key).cloned().unwrap_or_default();
                    let layout = workspace_layout.clone();
                    let policy = policy.clone();
                    let scope = ObligationScope::new(principal.clone(), workspace.clone());
                    // One instant per scope, taken inside the blocking half so
                    // the two derivations there cannot straddle a ripening.
                    let outcome = tokio::task::spawn_blocking(move || {
                        sweep_scope(&layout, &scope, &previous, &policy, Utc::now())
                    })
                    .await;

                    let mut snapshot = worker_health.snapshot.write().await;
                    snapshot.scopes_seen += 1;
                    match outcome {
                        Ok(Ok((report, next))) => {
                            snapshot.absorb(&report);
                            memories.insert(key, next);
                        },
                        // A scope that failed is counted and the tick
                        // continues, and its memory is left UNTOUCHED: a
                        // failed sweep saw a partial view at best, and
                        // adopting it as the next tick's "previous" would let
                        // an unreadable log look like a relationship that had
                        // gone away.
                        Ok(Err(error)) => {
                            snapshot.scopes_failed += 1;
                            warn!(target: LOG_TARGET, %error, %principal, %workspace, "obligation sweep failed for one scope");
                        },
                        Err(error) => {
                            snapshot.scopes_failed += 1;
                            warn!(target: LOG_TARGET, %error, %principal, %workspace, "obligation sweep task did not complete");
                        },
                    }
                }

                let mut snapshot = worker_health.snapshot.write().await;
                finalize_tick(&mut snapshot, &Utc::now().to_rfc3339());
            }
            info!(target: LOG_TARGET, "obligation sweep cancelled");
        });

        Self {
            handle: Some(handle),
            cancel,
            health,
        }
    }

    pub fn health(&self) -> ObligationSweepHealth {
        self.health.clone()
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle {
            let _ = handle.await;
        }
    }
}
