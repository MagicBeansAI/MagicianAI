//! Three coding budgets, resolved once and carried together.
//!
//! A single number used to do three jobs — bound one Pi turn, bound the whole
//! coding task, and notice a hang. Those are different questions with different
//! right answers, and collapsing them meant a build/test cycle on a large
//! repository ran out of clock mid-thought and failed indistinguishably from a
//! genuine failure.
//!
//! - [`ResolvedCodingBudgets`] is the single resolved structure. Five
//!   independently-read config values is how two of them end up disagreeing, so
//!   resolution happens once and the whole struct is passed down.
//! - [`ProgressWatchdog`] is the no-progress detector. It is **phase-aware**: a
//!   naive "any event resets a ten-minute timer" would kill a sixteen-minute
//!   silent test on this repository, and would equally be held open forever by
//!   an idle-chatter loop. Each phase carries its own deadline, and only
//!   *substantive* deltas advance progress.
//! - [`CodingTerminationReason`] replaces string-parsing a cancellation
//!   message. A token cannot explain why it fired, and the difference between
//!   "the repair failed" and "the repair never finished" is the whole point.
//!
//! Plan: `docs/archive/plans/2026-08-07-vibedev-run-duration.md`.

use std::{
    collections::{hash_map::DefaultHasher, HashMap, VecDeque},
    hash::{Hash, Hasher},
    sync::{Mutex, OnceLock, RwLock},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CodingEngineEvent, CodingEngineEventKind};
use crate::config::MagicianCodingSettings;

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Outer backstop for one Pi turn (8 h) — **not** a budget.
///
/// Duration is the wrong thing to bound. A turn that has been running for six
/// hours while committing every two minutes is healthy; a turn that has been
/// silent for twenty is not, regardless of how long it has been alive. The
/// bounds that decide a run's fate are the liveness ones below and the cost
/// ceiling; this exists only to catch a bug in one of those. Set to `0` for no
/// wall clock at all.
pub const DEFAULT_CODING_TURN_TIMEOUT_SECS: u64 = 8 * 60 * 60;

/// Whole-task active-time ceiling. **`0` = none, and that is the default.**
///
/// An earlier draft made this a 2 h ceiling justified by "repair needs room the
/// coding phase must not consume" — but starvation only exists *because* of a
/// ceiling, so the reserve existed purely to mitigate the ceiling it sat
/// inside. Removing the ceiling removes both. Active time is still accumulated
/// and reported; a positive value here turns that accounting into enforcement.
pub const DEFAULT_CODING_TASK_BUDGET_SECS: u64 = 0;

/// Silence from the model with no text or thinking delta. **This, not the wall
/// clock, is the bound that decides whether a run is alive.**
pub const DEFAULT_CODING_MODEL_IDLE_SECS: u64 = 15 * 60;

/// Silence from a running tool. Deliberately longer than the model's: a single
/// test command on this repository legitimately runs sixteen minutes emitting
/// nothing at all, and a generic ten-minute bound would kill it.
pub const DEFAULT_CODING_TOOL_IDLE_SECS: u64 = 25 * 60;

/// A tool that runs forever *while staying chatty* — the case an inactivity
/// bound alone can never catch.
pub const DEFAULT_CODING_TOOL_MAX_SECS: u64 = 50 * 60;

/// A compaction emits nothing while it runs, so it gets its own bound rather
/// than being measured against model silence.
pub const DEFAULT_CODING_COMPACTION_MAX_SECS: u64 = 15 * 60;

/// Same for a summarization retry, which is a second silent LLM round trip.
pub const DEFAULT_CODING_SUMMARIZATION_MAX_SECS: u64 = 15 * 60;

/// Added to a retry's **own declared** backoff. An auto-retry that announces a
/// nine-minute wait is not a hang, and charging it against model silence would
/// kill exactly the recovery it was performing.
pub const DEFAULT_CODING_RETRY_GRACE_SECS: u64 = 2 * 60;

/// Room held back from a whole-task ceiling so verification and repair cannot
/// be starved by the coding phase.
///
/// **Only meaningful when `task_budget_secs` is positive.** With no ceiling
/// there is nothing to be starved of: verification simply runs when coding
/// finishes.
pub const DEFAULT_CODING_VERIFICATION_RESERVE_SECS: u64 = 20 * 60;

/// What `turn_timeout_secs: 0` resolves to.
///
/// Not `Duration::MAX`: tokio's timer is not built for infinity, and threading
/// an `Option` through every deadline to express "never" buys nothing a year
/// does not. A run that reaches this has defeated the liveness detector, the
/// tool bounds, the loop detector and the cost ceiling.
pub const UNBOUNDED_CODING_TURN_SECS: u64 = 365 * 24 * 60 * 60;

/// Ceiling on an explicitly-configured turn backstop. Deliberately far above
/// anything a real turn reaches — the wall clock is no longer the mechanism
/// that decides when a run stops.
pub const MAX_CODING_TURN_TIMEOUT_SECS: u64 = UNBOUNDED_CODING_TURN_SECS;

/// How many recent event fingerprints are remembered inside one phase when
/// deciding whether a streaming update actually moved. One slot would only
/// catch immediately-repeated events, so a two-frame `A B A B` spinner would
/// keep a dead process alive forever.
const FINGERPRINT_MEMORY: usize = 8;

/// Node budget for the structural hash of an event payload. A tool result can
/// be megabytes and a run emits ~10k events; hashing every byte of every one is
/// not worth the fidelity.
const FINGERPRINT_NODE_BUDGET: usize = 512;

/// Bytes of any one string that reach the hash. The node budget bounds how many
/// *values* are visited but not how long each is, so a single multi-megabyte
/// tool result would otherwise be hashed in full on every streaming event.
/// Length is hashed alongside the prefix, so two payloads that share an opening
/// but differ in size still differ.
const FINGERPRINT_STRING_BYTES: usize = 128;

/// How often the drain loop asks whether the current phase has stalled.
///
/// Deliberately coarse. The bounds this guards are minutes wide, so arriving up
/// to one tick late costs nothing — while an exact per-event deadline would
/// register and tear down a timer for each of a run's ~10k events, and would
/// embed a `Sleep` in the enclosing future rather than behind a pointer.
const MAX_PROGRESS_CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// Unreachable from configuration — the tightest bound YAML can express is one
/// second, which yields a 250ms tick. The floor exists so a programmatically
/// constructed budget set cannot drive the timer arbitrarily fast.
const MIN_PROGRESS_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// Keys dropped before fingerprinting. Left in, a monotonic timestamp or a
/// fresh request id makes every event unique and the identical-event rule
/// silently becomes a no-op — the exact failure it exists to prevent.
const FINGERPRINT_VOLATILE_KEYS: &[&str] = &[
    "timestamp",
    "timestampMs",
    "timestamp_ms",
    "ts",
    "time",
    "startedAt",
    "started_at",
    "updatedAt",
    "updated_at",
    "finishedAt",
    "finished_at",
    "elapsed",
    "elapsedMs",
    "elapsed_ms",
    "durationMs",
    "duration_ms",
    "sequence",
    "requestId",
    "request_id",
    "messageId",
    "message_id",
];

// ---------------------------------------------------------------------------
// Phases and termination causes
// ---------------------------------------------------------------------------

/// Which kind of waiting the coding turn is currently doing. The phase decides
/// *which* deadline applies and what is allowed to advance it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingProgressPhase {
    /// Waiting on the model: advanced by a substantive text or thinking delta.
    Model,
    /// A tool is running: advanced by real output, bounded twice (inactivity
    /// *and* max runtime).
    Tool,
    /// A context compaction is running. Emits nothing; bounded by its own
    /// deadline, which events do not extend.
    Compaction,
    /// A summarization retry. Same shape as compaction.
    Summarization,
    /// Sleeping out a declared retry backoff. Bounded by that declared delay
    /// plus grace, never by model silence.
    RetryWait,
}

impl CodingProgressPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Tool => "tool",
            Self::Compaction => "compaction",
            Self::Summarization => "summarization",
            Self::RetryWait => "retry_wait",
        }
    }
}

/// Why a coding run stopped. Pi races against one cancellation token and
/// reports every external cancellation as "parent deadline/stop"; a token
/// cannot explain why it fired, and string-parsing the message afterwards is
/// not a contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum CodingTerminationReason {
    /// One Pi turn outlived `turn_timeout_secs`.
    TurnTimeout { limit_secs: u64 },
    /// The whole coding task exhausted its active-time budget.
    TaskBudget { limit_secs: u64, active_secs: u64 },
    /// Nothing substantive happened for the current phase's bound. Carries the
    /// phase because a silent model, a hung test and a wedged compaction are
    /// three different diagnoses, and collapsing them back into one is the
    /// problem this enum exists to solve.
    NoProgress {
        phase: CodingProgressPhase,
        elapsed_secs: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_substantive_event: Option<String>,
    },
    /// The operator pressed Stop.
    OwnerCancelled,
    /// A parent execution's deadline fired.
    ParentDeadline,
    /// A cost ceiling, not a clock, ended the run.
    CostBudget,
    /// The process is going away.
    ServiceShutdown,
}

impl CodingTerminationReason {
    /// Stable discriminant for telemetry and UI, independent of the payload.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::TurnTimeout { .. } => "turn_timeout",
            Self::TaskBudget { .. } => "task_budget",
            Self::NoProgress { .. } => "no_progress",
            Self::OwnerCancelled => "owner_cancelled",
            Self::ParentDeadline => "parent_deadline",
            Self::CostBudget => "cost_budget",
            Self::ServiceShutdown => "service_shutdown",
        }
    }

    /// True when the run was stopped by a budget rather than by failing. The
    /// verification controller needs this to tell "the repair failed" from "the
    /// repair never finished".
    pub fn is_budget_stop(&self) -> bool {
        matches!(
            self,
            Self::TurnTimeout { .. } | Self::TaskBudget { .. } | Self::NoProgress { .. }
        )
    }

    pub fn describe(&self) -> String {
        match self {
            Self::TurnTimeout { limit_secs } => {
                format!("the turn reached its {limit_secs}s wall clock")
            },
            Self::TaskBudget {
                limit_secs,
                active_secs,
            } => format!("the task reached its {limit_secs}s active budget ({active_secs}s spent)"),
            Self::NoProgress {
                phase,
                elapsed_secs,
                last_substantive_event,
            } => match last_substantive_event {
                Some(event) => format!(
                    "no progress in the {} phase for {elapsed_secs}s (last: {event})",
                    phase.as_str()
                ),
                None => format!(
                    "no progress in the {} phase for {elapsed_secs}s",
                    phase.as_str()
                ),
            },
            Self::OwnerCancelled => "the operator stopped the run".to_string(),
            Self::ParentDeadline => "a parent execution deadline fired".to_string(),
            Self::CostBudget => "the run reached its cost ceiling".to_string(),
            Self::ServiceShutdown => "the service is shutting down".to_string(),
        }
    }
}

/// Error carrying a typed termination cause out of the drain loop, so callers
/// can recover the reason by downcast instead of parsing a message.
#[derive(Debug, Clone)]
pub struct CodingTerminated {
    pub reason: CodingTerminationReason,
}

impl std::fmt::Display for CodingTerminated {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "coding run terminated: {}",
            self.reason.describe()
        )
    }
}

impl std::error::Error for CodingTerminated {}

// ---------------------------------------------------------------------------
// The resolved budgets
// ---------------------------------------------------------------------------

/// Every coding deadline, resolved together. Passed whole so two of them cannot
/// be read from different places and disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCodingBudgets {
    /// Outer backstop for one Pi turn. Not the mechanism that decides when a
    /// healthy run stops — that is the liveness pair below, plus cost.
    pub turn_max: Duration,
    /// Optional whole-task ceiling on ACTIVE time (HITL waiting excluded).
    /// `None` — the default — means active time is measured and reported but
    /// never used to stop work.
    pub task_active_max: Option<Duration>,
    /// Silent model.
    pub model_idle: Duration,
    /// Silent tool.
    pub tool_idle: Duration,
    /// A tool that runs forever even while chatty.
    pub tool_max: Duration,
    pub compaction_max: Duration,
    pub summarization_max: Duration,
    /// Added to the retry's own declared `delayMs`.
    pub retry_grace: Duration,
    /// Held back from `task_active_max` for verification and repair. Inert
    /// without a task ceiling.
    pub verification_reserve: Duration,
    /// Whether the phase-aware no-progress detector runs at all. The kill
    /// switch exists because a detector that mis-fires is worse than none —
    /// but with it off, only the wall clock and cost remain.
    pub no_progress_enabled: bool,
}

impl Default for ResolvedCodingBudgets {
    fn default() -> Self {
        Self {
            turn_max: Duration::from_secs(DEFAULT_CODING_TURN_TIMEOUT_SECS),
            task_active_max: None,
            model_idle: Duration::from_secs(DEFAULT_CODING_MODEL_IDLE_SECS),
            tool_idle: Duration::from_secs(DEFAULT_CODING_TOOL_IDLE_SECS),
            tool_max: Duration::from_secs(DEFAULT_CODING_TOOL_MAX_SECS),
            compaction_max: Duration::from_secs(DEFAULT_CODING_COMPACTION_MAX_SECS),
            summarization_max: Duration::from_secs(DEFAULT_CODING_SUMMARIZATION_MAX_SECS),
            retry_grace: Duration::from_secs(DEFAULT_CODING_RETRY_GRACE_SECS),
            verification_reserve: Duration::from_secs(DEFAULT_CODING_VERIFICATION_RESERVE_SECS),
            no_progress_enabled: true,
        }
    }
}

impl ResolvedCodingBudgets {
    /// Resolve from configuration. `requested_turn_secs` is the per-call
    /// override (the profile's own value, or the tool argument), clamped to the
    /// absolute outer bound — never silently widened past it.
    pub fn resolve(settings: &MagicianCodingSettings, requested_turn_secs: Option<u64>) -> Self {
        let no_progress = &settings.no_progress;
        let configured_turn_secs = resolve_turn_secs(settings.turn_timeout_secs);
        let turn_secs = requested_turn_secs
            .map(resolve_turn_secs)
            .unwrap_or(configured_turn_secs);
        Self {
            turn_max: Duration::from_secs(turn_secs),
            // Floored at one CONFIGURED turn, never one *requested* turn: a
            // caller asking for a longer turn must not thereby widen the whole
            // task's ceiling. An over-long request is narrowed by
            // `turn_max_within` instead. A ceiling below one turn would fail
            // every run on its first turn, which reads as a coding failure
            // rather than the configuration error it is.
            //
            // The floor applies only to a turn that is actually BOUNDED. An
            // unbounded turn resolves to a year, and flooring by that would
            // silently widen a deliberate two-hour task ceiling to a year —
            // turning "no turn wall clock" into "no task ceiling either".
            task_active_max: (settings.task_budget_secs > 0).then(|| {
                let turn_floor = if settings.turn_timeout_secs == 0 {
                    0
                } else {
                    configured_turn_secs
                };
                Duration::from_secs(settings.task_budget_secs.max(turn_floor))
            }),
            model_idle: Duration::from_secs(no_progress.model_idle_secs.max(1)),
            tool_idle: Duration::from_secs(no_progress.tool_idle_secs.max(1)),
            tool_max: Duration::from_secs(no_progress.tool_max_secs.max(1)),
            compaction_max: Duration::from_secs(no_progress.compaction_max_secs.max(1)),
            summarization_max: Duration::from_secs(no_progress.summarization_max_secs.max(1)),
            retry_grace: Duration::from_secs(no_progress.retry_grace_secs),
            verification_reserve: Duration::from_secs(settings.verification_reserve_secs),
            no_progress_enabled: no_progress.enabled,
        }
    }

    /// The slice of a task ceiling the coding phase may spend, with the
    /// verification reserve held back. `None` when there is no ceiling.
    ///
    /// Never zero: a reserve wider than the whole ceiling would otherwise make
    /// every run unrunnable.
    pub fn coding_phase_max(&self) -> Option<Duration> {
        let ceiling = self.task_active_max?;
        Some(
            ceiling
                .checked_sub(self.verification_reserve)
                .filter(|remaining| !remaining.is_zero())
                .unwrap_or(ceiling),
        )
    }

    /// Narrow the turn to what a task ceiling actually has left, so the last
    /// turn of a long task cannot overshoot it. Unchanged when there is no
    /// ceiling.
    pub fn turn_max_within(&self, task_active_spent: Duration) -> Duration {
        let Some(coding_max) = self.coding_phase_max() else {
            return self.turn_max;
        };
        let remaining = coding_max
            .checked_sub(task_active_spent)
            .unwrap_or_default();
        self.turn_max.min(remaining)
    }

    /// Wall-clock ceiling for one coding *execution*, which may run several
    /// turns.
    ///
    /// `None` with no task ceiling — the default.
    ///
    /// An execution runs several turns, each already bounded by its own wall
    /// clock and by liveness, so a separate execution-level clock adds nothing
    /// except a way to kill the third four-hour turn mid-flight. Returning
    /// `None` also means no watchdog is armed at all, rather than one that
    /// sleeps for a year.
    ///
    /// When a ceiling IS configured, the wider of the two applies — an
    /// execution must never be cut short by a limit narrower than the turn it
    /// is currently running.
    pub fn execution_ceiling(&self) -> Option<Duration> {
        let ceiling = self.task_active_max?;
        // An unbounded turn may legitimately outlast any execution-level clock,
        // so there is nothing sane to bound the execution by — and a watchdog
        // armed for a year is a task nobody wants either.
        if self.turn_max.as_secs() >= UNBOUNDED_CODING_TURN_SECS {
            return None;
        }
        Some(ceiling.max(self.turn_max))
    }

    /// How often the drain loop should test for a stall.
    ///
    /// A quarter of the tightest phase bound, bounded by
    /// [`MIN_PROGRESS_CHECK_INTERVAL`]/[`MAX_PROGRESS_CHECK_INTERVAL`].
    /// `retry_grace` is deliberately excluded: it is an additive term on a
    /// declared backoff rather than a bound in its own right, and configuring
    /// it to zero would otherwise drive the tick to its floor for no gain.
    pub fn check_interval(&self) -> Duration {
        let tightest = self
            .model_idle
            .min(self.tool_idle)
            .min(self.compaction_max)
            .min(self.summarization_max);
        (tightest / 4).clamp(MIN_PROGRESS_CHECK_INTERVAL, MAX_PROGRESS_CHECK_INTERVAL)
    }

    /// Telemetry payload for the task card, so a long run reads as *working*
    /// rather than *stuck*.
    ///
    /// With no task ceiling, spend is still reported — it is genuinely useful
    /// to know a task has spent four active hours — but `remaining` is omitted
    /// rather than reported as zero, which would read as "out of budget" when
    /// there is no budget to be out of.
    pub fn telemetry(&self, task_active_spent: Duration) -> Value {
        let coding_max = self.coding_phase_max();
        // An unbounded turn reports as absent rather than as 31536000, which
        // would render on a card as a meaningless year-long "budget".
        let turn_max_secs =
            (self.turn_max.as_secs() < UNBOUNDED_CODING_TURN_SECS).then(|| self.turn_max.as_secs());
        serde_json::json!({
            "turn_max_secs": turn_max_secs,
            "task_active_max_secs": self.task_active_max.map(|max| max.as_secs()),
            "coding_phase_max_secs": coding_max.map(|max| max.as_secs()),
            // The reserve as ACTUALLY applied. A reserve wider than the whole
            // ceiling is ignored (§`coding_phase_max`), and reporting the
            // configured value there would claim time was held back that never
            // was.
            "verification_reserve_secs": self.task_active_max.zip(coding_max).map(
                |(ceiling, coding_max)| ceiling.as_secs().saturating_sub(coding_max.as_secs()),
            ),
            "task_active_spent_secs": task_active_spent.as_secs(),
            "task_active_remaining_secs": coding_max
                .map(|max| max.as_secs().saturating_sub(task_active_spent.as_secs())),
            "no_progress_enabled": self.no_progress_enabled,
            "model_idle_secs": self.model_idle.as_secs(),
        })
    }
}

/// `0` means "no wall clock" — resolved to a year rather than threaded as an
/// `Option` through every deadline, since nothing distinguishes the two.
fn resolve_turn_secs(raw: u64) -> u64 {
    if raw == 0 {
        UNBOUNDED_CODING_TURN_SECS
    } else {
        raw.min(MAX_CODING_TURN_TIMEOUT_SECS)
    }
}

/// Bound a configured or requested turn backstop **while preserving the `0`
/// that means "no wall clock"**.
///
/// A plain `clamp(1, MAX)` turns that `0` into ONE SECOND — so the very setting
/// documented for long autonomous runs would kill every turn on its first
/// await. Any layer that narrows a turn value must go through here.
pub fn clamp_turn_timeout_secs(raw: u64) -> u64 {
    if raw == 0 {
        0
    } else {
        raw.min(MAX_CODING_TURN_TIMEOUT_SECS)
    }
}

// ---------------------------------------------------------------------------
// Process-wide resolved policy
// ---------------------------------------------------------------------------

fn coding_settings_cell() -> &'static RwLock<MagicianCodingSettings> {
    static CELL: OnceLock<RwLock<MagicianCodingSettings>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(MagicianCodingSettings::default()))
}

/// Publish the coding budget policy for readers that hold no config snapshot —
/// notably the agentic executor's wall-clock watchdog, which must move in
/// lockstep with the Pi turn clip or one ceiling gets lifted and the other does
/// not. Called wherever the Magician config is loaded or reloaded.
pub fn configure_coding_budgets(settings: &MagicianCodingSettings) {
    match coding_settings_cell().write() {
        Ok(mut guard) => *guard = settings.clone(),
        Err(error) => {
            tracing::warn!(
                target: "coding_engine",
                %error,
                "failed to publish coding budgets; keeping the previous policy"
            );
        },
    }
}

/// The published policy, or defaults before the first config load.
pub fn coding_budget_settings() -> MagicianCodingSettings {
    match coding_settings_cell().read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

// ---------------------------------------------------------------------------
// First-writer-wins termination registry
// ---------------------------------------------------------------------------

/// The executor's deadline watchdog runs in a spawned task and cancels a token
/// the Pi turn is racing against; the two cannot share a stack, so the reason
/// travels beside the token through a registry keyed by execution id. First
/// writer wins — the cause that actually fired is the one that stays.
struct TerminationRegistry {
    reasons: HashMap<String, CodingTerminationReason>,
    order: VecDeque<String>,
}

/// Bound so a long-lived process cannot accumulate an entry per execution
/// forever if a caller ever forgets to clear one.
const TERMINATION_REGISTRY_CAPACITY: usize = 512;

fn termination_registry() -> &'static Mutex<TerminationRegistry> {
    static CELL: OnceLock<Mutex<TerminationRegistry>> = OnceLock::new();
    CELL.get_or_init(|| {
        Mutex::new(TerminationRegistry {
            reasons: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

/// A poisoned registry still holds usable data — losing every termination
/// reason because one thread panicked elsewhere would turn a diagnosable stop
/// back into an unexplained one.
fn with_registry<T>(apply: impl FnOnce(&mut TerminationRegistry) -> T) -> T {
    match termination_registry().lock() {
        Ok(mut guard) => apply(&mut guard),
        Err(poisoned) => apply(&mut poisoned.into_inner()),
    }
}

/// Record why this execution is being stopped. Returns `true` when this call
/// was the first writer; a later, more generic cause never overwrites the
/// specific one that actually fired.
pub fn record_termination_reason(execution_id: &str, reason: CodingTerminationReason) -> bool {
    if execution_id.is_empty() {
        return false;
    }
    with_registry(|registry| {
        if registry.reasons.contains_key(execution_id) {
            return false;
        }
        if registry.order.len() >= TERMINATION_REGISTRY_CAPACITY {
            if let Some(evicted) = registry.order.pop_front() {
                registry.reasons.remove(&evicted);
            }
        }
        registry.order.push_back(execution_id.to_string());
        registry.reasons.insert(execution_id.to_string(), reason);
        true
    })
}

pub fn termination_reason(execution_id: &str) -> Option<CodingTerminationReason> {
    if execution_id.is_empty() {
        return None;
    }
    with_registry(|registry| registry.reasons.get(execution_id).cloned())
}

/// Drop this execution's entry. Called when the run settles, so the registry
/// tracks live executions rather than growing to its eviction bound.
pub fn clear_termination_reason(execution_id: &str) {
    if execution_id.is_empty() {
        return;
    }
    with_registry(|registry| {
        registry.reasons.remove(execution_id);
        if let Some(position) = registry.order.iter().position(|id| id == execution_id) {
            registry.order.remove(position);
        }
    });
}

// ---------------------------------------------------------------------------
// The phase-aware watchdog
// ---------------------------------------------------------------------------

/// Tracks which phase the turn is in and whether anything substantive has
/// happened inside it.
///
/// Two failure modes this exists to avoid, both of which a single "last event
/// timestamp" produces:
///
/// 1. **Killing healthy work.** A sixteen-minute test emits
///    `ToolExecutionStart` and then goes silent; a compaction emits nothing at
///    all; an auto-retry announces its own multi-minute backoff. Measured
///    against a generic idle timer, every one of those is a hang.
/// 2. **Holding a dead process open.** Repeated empty or identical
///    `ToolExecutionUpdate` events would reset a naive timer forever.
pub struct ProgressWatchdog {
    budgets: ResolvedCodingBudgets,
    phase: CodingProgressPhase,
    /// When the current phase began. Anchors the phase-owned deadlines
    /// (compaction, summarization, retry wait) that events must NOT extend.
    phase_started: Instant,
    /// Last event that counted as real progress. Anchors the idle deadlines.
    last_substantive_at: Instant,
    /// What that event was. Split into a static kind name and a tool name so
    /// the common case allocates nothing: a run streams ~10k events and only a
    /// handful of them change the tool.
    last_substantive_kind: Option<CodingEngineEventKind>,
    last_substantive_tool: Option<String>,
    /// Tool executions currently in flight, by the id Pi correlates them with.
    ///
    /// Pi runs tools concurrently — `toolCallId` exists on all three tool
    /// events precisely so overlapping executions can be told apart.
    /// Returning to the model phase when *one* of them ends would measure a
    /// still-running silent sixteen-minute test against the model's much
    /// tighter idle bound, which is the exact regression this detector
    /// exists to prevent. A `Vec` rather than a set: there are a handful of
    /// these at a time, so a linear scan beats hashing and allocates once.
    active_tools: Vec<String>,
    /// When the FIRST of the currently-active tools started. Anchors the
    /// max-runtime bound, so a later sibling starting cannot reset the clock on
    /// a tool that has already been running for forty minutes.
    tool_started_at: Option<Instant>,
    /// Absolute cap for the current tool phase, independent of how chatty it
    /// is.
    tool_deadline: Option<Instant>,
    /// A backoff the current phase declared for itself.
    declared_wait: Duration,
    /// Recent payload fingerprints within the current phase.
    fingerprints: VecDeque<u64>,
    /// Time spent in declared retry backoff, which is not active work.
    backoff_total: Duration,
}

impl ProgressWatchdog {
    pub fn new(budgets: ResolvedCodingBudgets, now: Instant) -> Self {
        Self {
            budgets,
            phase: CodingProgressPhase::Model,
            phase_started: now,
            last_substantive_at: now,
            last_substantive_kind: None,
            last_substantive_tool: None,
            active_tools: Vec::new(),
            tool_started_at: None,
            tool_deadline: None,
            declared_wait: Duration::ZERO,
            fingerprints: VecDeque::new(),
            backoff_total: Duration::ZERO,
        }
    }

    pub fn phase(&self) -> CodingProgressPhase {
        self.phase
    }

    /// How often the drain loop should ask this detector whether it has given
    /// up. Delegated so the caller does not need to carry the budgets too.
    pub fn check_interval(&self) -> Duration {
        self.budgets.check_interval()
    }

    /// Total time spent sleeping out declared provider backoff. Excluded from
    /// active work — a rate-limit wait is not the agent thinking — while still
    /// bounded by the retry-wait phase, so a queue stall is not free either.
    pub fn backoff_total(&self) -> Duration {
        let mut total = self.backoff_total;
        if self.phase == CodingProgressPhase::RetryWait {
            // Still waiting when the turn was torn down: count what has elapsed
            // so far. `saturating_duration_since` rather than `elapsed()` so a
            // test-injected future instant reads as zero instead of panicking.
            total =
                total.saturating_add(Instant::now().saturating_duration_since(self.phase_started));
        }
        total
    }

    /// The instant at which the current phase gives up, and what that deadline
    /// is measured from.
    fn deadline_with_anchor(&self) -> Option<(Instant, Instant)> {
        if !self.budgets.no_progress_enabled {
            return None;
        }
        // `checked_add` throughout: a bound wide enough to overflow the clock is
        // a nonsense configuration, and the right response is "never fires", not
        // a panic that takes the coding run down with it.
        match self.phase {
            CodingProgressPhase::Model => Some((
                self.last_substantive_at
                    .checked_add(self.budgets.model_idle)?,
                self.last_substantive_at,
            )),
            CodingProgressPhase::Tool => {
                let idle = self.last_substantive_at.checked_add(self.budgets.tool_idle);
                // Anchored on the first tool of the batch, so the max-runtime
                // bound measures the tool that has actually been running longest.
                let max_anchor = self.tool_started_at.unwrap_or(self.phase_started);
                match (self.tool_deadline, idle) {
                    // Whichever bound bites first also decides what the reported
                    // elapsed time is measured against, so the diagnosis names
                    // the bound that actually fired.
                    (Some(max), Some(idle)) if max <= idle => Some((max, max_anchor)),
                    (_, Some(idle)) => Some((idle, self.last_substantive_at)),
                    (Some(max), None) => Some((max, max_anchor)),
                    (None, None) => None,
                }
            },
            CodingProgressPhase::Compaction => Some((
                self.phase_started
                    .checked_add(self.budgets.compaction_max)?,
                self.phase_started,
            )),
            CodingProgressPhase::Summarization => {
                // A scheduled retry may declare a wait longer than the generic
                // summarization bound; honour the longer of the two rather than
                // killing a retry that told us how long it would take.
                let declared = self.declared_wait.saturating_add(self.budgets.retry_grace);
                let bound = self.budgets.summarization_max.max(declared);
                Some((self.phase_started.checked_add(bound)?, self.phase_started))
            },
            CodingProgressPhase::RetryWait => {
                let bound = self.declared_wait.saturating_add(self.budgets.retry_grace);
                Some((self.phase_started.checked_add(bound)?, self.phase_started))
            },
        }
    }

    /// When the current phase gives up, or `None` when the detector is off.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline_with_anchor().map(|(deadline, _)| deadline)
    }

    /// The reason this phase would report, whether or not it has expired.
    pub fn no_progress_reason(&self, now: Instant) -> CodingTerminationReason {
        let anchor = self
            .deadline_with_anchor()
            .map(|(_, anchor)| anchor)
            .unwrap_or(self.last_substantive_at);
        CodingTerminationReason::NoProgress {
            phase: self.phase,
            elapsed_secs: now.saturating_duration_since(anchor).as_secs(),
            last_substantive_event: self.last_substantive_label(),
        }
    }

    /// Build the diagnostic label. Called once, when a phase gives up — not on
    /// the streaming path.
    fn last_substantive_label(&self) -> Option<String> {
        let kind = self.last_substantive_kind?.as_str();
        Some(match self.last_substantive_tool.as_deref() {
            Some(tool) => format!("{kind} ({tool})"),
            None => kind.to_string(),
        })
    }

    /// `Some(reason)` once the current phase's deadline has passed.
    pub fn expired(&self, now: Instant) -> Option<CodingTerminationReason> {
        let (deadline, _) = self.deadline_with_anchor()?;
        (now >= deadline).then(|| self.no_progress_reason(now))
    }

    /// Fold one engine event into the detector.
    pub fn observe(&mut self, event: &CodingEngineEvent, now: Instant) {
        use CodingEngineEventKind as Kind;
        match event.kind {
            // ---- structural transitions: always progress, always re-phase ----
            Kind::ToolExecutionStart => {
                let key = tool_key(event);
                if !self.active_tools.iter().any(|active| active == &key) {
                    self.active_tools.push(key);
                }
                // Anchor the max-runtime bound on the FIRST tool of this batch.
                if self.tool_started_at.is_none() {
                    self.tool_started_at = Some(now);
                    self.tool_deadline = now.checked_add(self.budgets.tool_max);
                }
                self.enter(CodingProgressPhase::Tool, event, now);
            },
            Kind::ToolExecutionEnd => {
                let key = tool_key(event);
                self.active_tools.retain(|active| active != &key);
                if self.active_tools.is_empty() {
                    self.tool_started_at = None;
                    self.tool_deadline = None;
                    self.enter(CodingProgressPhase::Model, event, now);
                } else {
                    // A sibling finished while others are still running. That is
                    // progress, but the tool phase still owns the clock.
                    self.enter(CodingProgressPhase::Tool, event, now);
                }
            },
            Kind::CompactionStart => self.enter(CodingProgressPhase::Compaction, event, now),
            Kind::CompactionEnd => self.enter(CodingProgressPhase::Model, event, now),
            Kind::AutoRetryStart => {
                let declared = declared_delay(&event.raw);
                self.enter(CodingProgressPhase::RetryWait, event, now);
                // Set AFTER entering: `enter` resets the declared wait, and this
                // phase's whole deadline is that declared backoff.
                self.declared_wait = declared.unwrap_or(self.budgets.model_idle);
            },
            // `enter` banks the backoff on the way out of a retry wait; doing it
            // here as well would charge the same wait twice.
            Kind::AutoRetryEnd => self.enter(CodingProgressPhase::Model, event, now),
            Kind::SummarizationRetryScheduled | Kind::SummarizationRetryAttemptStart => {
                let declared = declared_delay(&event.raw);
                self.enter(CodingProgressPhase::Summarization, event, now);
                self.declared_wait = declared.unwrap_or_default();
            },
            Kind::SummarizationRetryFinished => self.enter(CodingProgressPhase::Model, event, now),
            Kind::TurnStart
            | Kind::TurnEnd
            | Kind::MessageStart
            | Kind::MessageEnd
            | Kind::AgentStart
            | Kind::AgentEnd
            | Kind::AgentSettled
            | Kind::Response => {
                // Message and turn boundaries interleave with running tools. If
                // one of them dropped us back to the model phase, a silent tool
                // would immediately be measured against the model's bound. The
                // tool phase owns the clock until the last tool ends.
                let target = if self.active_tools.is_empty() {
                    CodingProgressPhase::Model
                } else {
                    CodingProgressPhase::Tool
                };
                self.enter(target, event, now);
            },

            // ---- streaming: progress only when the payload actually moved ----
            Kind::MessageUpdate
            | Kind::ToolExecutionUpdate
            | Kind::BashExecutionUpdate
            | Kind::EntryAppended => {
                if self.is_substantive(event) {
                    self.advance(event, now);
                }
            },

            // ---- notifications that carry no work signal ----
            // Deliberately inert. These are exactly the events an idle-chatter
            // loop produces, and treating them as progress is what would hold a
            // dead process open for the full turn.
            Kind::QueueUpdate
            | Kind::SessionInfoChanged
            | Kind::ThinkingLevelChanged
            | Kind::ExtensionError
            | Kind::Unknown => {},
        }
    }

    fn enter(&mut self, phase: CodingProgressPhase, event: &CodingEngineEvent, now: Instant) {
        // Bank any backoff the phase being left had accrued. Unconditional
        // because `charge_backoff` is a no-op outside a retry wait, and because
        // a retry that re-enters a retry wait would otherwise have its first
        // wait silently reset away by the new `phase_started`.
        self.charge_backoff(now);
        self.phase = phase;
        self.phase_started = now;
        self.declared_wait = Duration::ZERO;
        self.fingerprints.clear();
        if phase != CodingProgressPhase::Tool {
            // A deliberate move to compaction or a retry wait abandons tool
            // tracking rather than leaving a stale set that would keep the
            // detector in the tool phase forever.
            self.tool_deadline = None;
            self.tool_started_at = None;
            self.active_tools.clear();
        }
        self.advance(event, now);
    }

    fn advance(&mut self, event: &CodingEngineEvent, now: Instant) {
        self.last_substantive_at = now;
        self.last_substantive_kind = Some(event.kind);
        // Only re-allocate when the tool actually changes; clearing is free.
        match event.tool_name.as_deref().filter(|tool| !tool.is_empty()) {
            Some(tool) => {
                if self.last_substantive_tool.as_deref() != Some(tool) {
                    self.last_substantive_tool = Some(tool.to_string());
                }
            },
            None => self.last_substantive_tool = None,
        }
    }

    fn charge_backoff(&mut self, now: Instant) {
        if self.phase == CodingProgressPhase::RetryWait {
            self.backoff_total = self
                .backoff_total
                .saturating_add(now.saturating_duration_since(self.phase_started));
        }
    }

    /// Whether a streaming event moved anything. Empty deltas and payloads
    /// identical to one already seen in this phase reset nothing.
    fn is_substantive(&mut self, event: &CodingEngineEvent) -> bool {
        if event.kind == CodingEngineEventKind::MessageUpdate {
            let text = event.text_delta.as_deref().unwrap_or("");
            let thinking = event.thinking_delta.as_deref().unwrap_or("");
            if text.is_empty() && thinking.is_empty() {
                return false;
            }
        }
        let fingerprint = fingerprint_event(event);
        if self.fingerprints.contains(&fingerprint) {
            return false;
        }
        if self.fingerprints.len() >= FINGERPRINT_MEMORY {
            self.fingerprints.pop_front();
        }
        self.fingerprints.push_back(fingerprint);
        true
    }
}

/// How a tool execution is correlated across its start/update/end events.
///
/// `toolCallId` is what Pi actually correlates on. The fallbacks only matter if
/// it is ever absent, and they degrade to treating concurrent anonymous tools
/// as one — which is the pre-existing behaviour, not a new failure.
fn tool_key(event: &CodingEngineEvent) -> String {
    event
        .tool_call_id
        .as_deref()
        .or(event.tool_name.as_deref())
        .filter(|key| !key.is_empty())
        .unwrap_or("__anonymous_tool")
        .to_string()
}

/// A retry's own declared backoff, in whichever shape Pi reports it. Missing is
/// meaningfully different from zero: an undeclared retry falls back to a
/// generous bound rather than being killed immediately.
fn declared_delay(raw: &Value) -> Option<Duration> {
    const MILLISECOND_KEYS: &[&str] = &["delayMs", "delay_ms", "retryDelayMs", "retry_delay_ms"];
    const SECOND_KEYS: &[&str] = &["delaySecs", "delay_secs", "retryAfterSecs", "retry_after"];
    for key in MILLISECOND_KEYS {
        if let Some(millis) = lookup_number(raw, key) {
            return Some(Duration::from_millis(millis));
        }
    }
    for key in SECOND_KEYS {
        if let Some(secs) = lookup_number(raw, key) {
            return Some(Duration::from_secs(secs));
        }
    }
    None
}

/// Look the key up at the top level, then one level down under the containers
/// Pi nests retry metadata in.
fn lookup_number(raw: &Value, key: &str) -> Option<u64> {
    const NESTED: &[&str] = &["retry", "backoff", "schedule", "data"];
    let direct = raw.get(key).and_then(Value::as_u64);
    if direct.is_some() {
        return direct;
    }
    NESTED
        .iter()
        .find_map(|container| raw.get(container)?.get(key)?.as_u64())
}

/// Structural hash of an event, with volatile keys removed so a clock or a
/// fresh id cannot make every repeat look new.
fn fingerprint_event(event: &CodingEngineEvent) -> u64 {
    let mut hasher = DefaultHasher::new();
    event.kind.hash(&mut hasher);
    for field in [
        event.raw_type.as_deref(),
        event.text_delta.as_deref(),
        event.thinking_delta.as_deref(),
        event.tool_name.as_deref(),
        event.tool_call_id.as_deref(),
        event.assistant_event_type.as_deref(),
        event.stop_reason.as_deref(),
        event.error_message.as_deref(),
    ] {
        hash_optional_str(field, &mut hasher);
    }
    let mut budget = FINGERPRINT_NODE_BUDGET;
    hash_value(&event.raw, &mut hasher, &mut budget);
    hasher.finish()
}

/// Hash a bounded prefix plus the full length, so a long payload costs the same
/// as a short one while two payloads that differ in size still differ.
fn hash_str_bounded(text: &str, hasher: &mut DefaultHasher) {
    text.len().hash(hasher);
    let head = text.len().min(FINGERPRINT_STRING_BYTES);
    text.as_bytes()[..head].hash(hasher);
}

fn hash_optional_str(text: Option<&str>, hasher: &mut DefaultHasher) {
    match text {
        Some(text) => {
            1u8.hash(hasher);
            hash_str_bounded(text, hasher);
        },
        None => 0u8.hash(hasher),
    }
}

fn hash_value(value: &Value, hasher: &mut DefaultHasher, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(flag) => {
            1u8.hash(hasher);
            flag.hash(hasher);
        },
        Value::Number(number) => {
            2u8.hash(hasher);
            number.to_string().hash(hasher);
        },
        Value::String(text) => {
            3u8.hash(hasher);
            hash_str_bounded(text, hasher);
        },
        Value::Array(items) => {
            4u8.hash(hasher);
            for item in items {
                hash_value(item, hasher, budget);
                if *budget == 0 {
                    return;
                }
            }
        },
        Value::Object(entries) => {
            5u8.hash(hasher);
            // BTreeMap iteration is already ordered, so the hash is stable
            // across payloads that differ only in key order.
            for (key, item) in entries {
                if FINGERPRINT_VOLATILE_KEYS.contains(&key.as_str()) {
                    continue;
                }
                key.hash(hasher);
                hash_value(item, hasher, budget);
                if *budget == 0 {
                    return;
                }
            }
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    /// Build events the way production does — through `coding_event_from_raw`,
    /// so the kind mapping and the delta extraction under test are the real
    /// ones rather than a hand-assembled struct that could drift from them.
    fn raw_event(raw_type: &str, mut raw: Value) -> CodingEngineEvent {
        if let Some(object) = raw.as_object_mut() {
            object.insert("type".to_string(), Value::String(raw_type.to_string()));
        }
        super::super::coding_event_from_raw(1, raw)
    }

    fn budgets() -> ResolvedCodingBudgets {
        ResolvedCodingBudgets {
            turn_max: Duration::from_secs(3600),
            task_active_max: Some(Duration::from_secs(7200)),
            model_idle: Duration::from_secs(600),
            tool_idle: Duration::from_secs(1500),
            tool_max: Duration::from_secs(3000),
            compaction_max: Duration::from_secs(900),
            summarization_max: Duration::from_secs(900),
            retry_grace: Duration::from_secs(120),
            verification_reserve: Duration::from_secs(1200),
            no_progress_enabled: true,
        }
    }

    #[test]
    fn a_sixteen_minute_silent_tool_is_not_a_hang() {
        // The exact case a generic ten-minute idle timer would kill: a test
        // command that starts, says nothing for sixteen minutes, and finishes.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        let sixteen_minutes = start + Duration::from_secs(16 * 60);
        assert_eq!(watchdog.expired(sixteen_minutes), None);
        watchdog.observe(
            &raw_event("tool_execution_end", json!({"toolName": "bash"})),
            sixteen_minutes,
        );
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
    }

    #[test]
    fn a_sibling_tool_ending_does_not_expose_a_running_one_to_the_model_bound() {
        // Pi runs tools concurrently. If one ending dropped the detector back to
        // the model phase, a still-running silent sixteen-minute test would be
        // measured against the 10-minute model bound and killed — the exact
        // regression this detector exists to prevent.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event(
                "tool_execution_start",
                json!({"toolCallId": "slow", "toolName": "bash"}),
            ),
            start,
        );
        watchdog.observe(
            &raw_event(
                "tool_execution_start",
                json!({"toolCallId": "quick", "toolName": "read"}),
            ),
            start,
        );
        watchdog.observe(
            &raw_event(
                "tool_execution_end",
                json!({"toolCallId": "quick", "toolName": "read"}),
            ),
            start + Duration::from_secs(1),
        );
        assert_eq!(
            watchdog.phase(),
            CodingProgressPhase::Tool,
            "the slow tool is still running"
        );
        // Well past model_idle (600s), comfortably inside tool_idle (1500s).
        assert_eq!(watchdog.expired(start + Duration::from_secs(960)), None);
        watchdog.observe(
            &raw_event(
                "tool_execution_end",
                json!({"toolCallId": "slow", "toolName": "bash"}),
            ),
            start + Duration::from_secs(960),
        );
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
    }

    #[test]
    fn a_message_boundary_during_a_tool_does_not_leave_the_tool_phase() {
        // Message and turn events interleave with running tools.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event(
                "tool_execution_start",
                json!({"toolCallId": "t1", "toolName": "bash"}),
            ),
            start,
        );
        for raw_type in ["message_end", "turn_end", "message_start"] {
            watchdog.observe(&raw_event(raw_type, json!({})), start);
            assert_eq!(watchdog.phase(), CodingProgressPhase::Tool, "{raw_type}");
        }
        assert_eq!(watchdog.expired(start + Duration::from_secs(960)), None);
    }

    #[test]
    fn a_later_sibling_cannot_reset_a_long_running_tools_max_bound() {
        // Anchoring max-runtime on the newest tool would let a chatty agent
        // start a cheap sibling every few minutes and keep a wedged command
        // alive forever.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event(
                "tool_execution_start",
                json!({"toolCallId": "wedged", "toolName": "bash"}),
            ),
            start,
        );
        for step in 1..=50 {
            let now = start + Duration::from_secs(step * 60);
            watchdog.observe(
                &raw_event(
                    "tool_execution_start",
                    json!({"toolCallId": format!("sibling-{step}"), "toolName": "read"}),
                ),
                now,
            );
        }
        // tool_max is 3000s, anchored on `wedged`, so it must have fired by now.
        assert!(
            watchdog
                .expired(start + Duration::from_secs(3001))
                .is_some(),
            "the first tool's max-runtime bound must survive later siblings"
        );
    }

    #[test]
    fn tools_without_a_call_id_still_open_and_close() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        assert_eq!(watchdog.phase(), CodingProgressPhase::Tool);
        watchdog.observe(
            &raw_event("tool_execution_end", json!({"toolName": "bash"})),
            start + Duration::from_secs(5),
        );
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
    }

    #[test]
    fn a_repeated_start_for_one_tool_does_not_wedge_the_phase() {
        // A duplicate start must not leave a phantom entry that keeps the tool
        // phase alive after the real end arrives.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        let event = raw_event(
            "tool_execution_start",
            json!({"toolCallId": "t1", "toolName": "bash"}),
        );
        watchdog.observe(&event, start);
        watchdog.observe(&event, start);
        watchdog.observe(
            &raw_event(
                "tool_execution_end",
                json!({"toolCallId": "t1", "toolName": "bash"}),
            ),
            start + Duration::from_secs(1),
        );
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
    }

    #[test]
    fn compaction_during_a_tool_abandons_tool_tracking_cleanly() {
        // Otherwise a stale active-tool set would hold the detector in the tool
        // phase for the rest of the turn.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event(
                "tool_execution_start",
                json!({"toolCallId": "t1", "toolName": "bash"}),
            ),
            start,
        );
        watchdog.observe(&raw_event("compaction_start", json!({})), start);
        watchdog.observe(&raw_event("compaction_end", json!({})), start);
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
        assert!(watchdog.expired(start + Duration::from_secs(601)).is_some());
    }

    #[test]
    fn an_absurd_bound_disables_the_deadline_instead_of_panicking() {
        // `Instant + Duration` panics on overflow. A nonsense config should mean
        // "never fires", not a crash that takes the coding run with it.
        let mut budgets = budgets();
        budgets.model_idle = Duration::from_secs(u64::MAX);
        let start = Instant::now();
        let watchdog = ProgressWatchdog::new(budgets, start);
        assert_eq!(watchdog.deadline(), None);
        assert_eq!(watchdog.expired(start + Duration::from_secs(10_000)), None);
    }

    #[test]
    fn a_silent_tool_past_its_inactivity_bound_is_a_hang() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        let past = start + Duration::from_secs(1501);
        let reason = watchdog.expired(past).expect("tool idle bound fires");
        assert!(matches!(
            reason,
            CodingTerminationReason::NoProgress {
                phase: CodingProgressPhase::Tool,
                ..
            }
        ));
    }

    #[test]
    fn a_chatty_tool_still_dies_at_its_max_runtime() {
        // Inactivity alone can never catch a tool that keeps talking. The
        // second bound is the one that does.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        for step in 1..=60 {
            let now = start + Duration::from_secs(step * 60);
            watchdog.observe(
                &raw_event(
                    "tool_execution_update",
                    json!({"toolName": "bash", "output": format!("line {step}")}),
                ),
                now,
            );
            if watchdog.expired(now).is_some() {
                break;
            }
        }
        let reason = watchdog
            .expired(start + Duration::from_secs(3001))
            .expect("tool max runtime fires despite constant output");
        assert!(matches!(
            reason,
            CodingTerminationReason::NoProgress {
                phase: CodingProgressPhase::Tool,
                ..
            }
        ));
    }

    #[test]
    fn identical_tool_updates_extend_nothing() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        // The same payload every thirty seconds for half an hour. A naive
        // detector would treat each as progress and never fire.
        let mut now = start;
        for step in 1..=60 {
            now = start + Duration::from_secs(step * 30);
            watchdog.observe(
                &raw_event("tool_execution_update", json!({"toolName": "bash"})),
                now,
            );
        }
        assert!(
            watchdog.expired(now).is_some(),
            "identical chatter must not hold a dead process open"
        );
    }

    #[test]
    fn a_two_frame_spinner_does_not_hold_the_process_open() {
        // `A B A B` defeats a one-slot memory. The ring is why it does not.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        let mut now = start;
        for step in 1..=120 {
            now = start + Duration::from_secs(step * 15);
            let frame = if step % 2 == 0 { "|" } else { "/" };
            watchdog.observe(
                &raw_event(
                    "tool_execution_update",
                    json!({"toolName": "bash", "output": frame}),
                ),
                now,
            );
        }
        assert!(watchdog.expired(now).is_some());
    }

    #[test]
    fn a_volatile_timestamp_does_not_disguise_a_repeat() {
        // If the fingerprint included the clock, every repeat would look new
        // and the identical-event rule would silently be a no-op.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        let mut now = start;
        for step in 1..=60 {
            now = start + Duration::from_secs(step * 30);
            watchdog.observe(
                &raw_event(
                    "tool_execution_update",
                    json!({"toolName": "bash", "timestampMs": step * 30_000}),
                ),
                now,
            );
        }
        assert!(watchdog.expired(now).is_some());
    }

    #[test]
    fn real_output_keeps_a_long_tool_alive() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        for step in 1..=40 {
            let now = start + Duration::from_secs(step * 60);
            watchdog.observe(
                &raw_event(
                    "tool_execution_update",
                    json!({"toolName": "bash", "output": format!("test {step} passed")}),
                ),
                now,
            );
            // Never idle-expires: each payload is genuinely different.
            if now < start + Duration::from_secs(3000) {
                assert_eq!(watchdog.expired(now), None, "step {step}");
            }
        }
    }

    #[test]
    fn empty_message_deltas_are_not_progress() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        let mut now = start;
        for step in 1..=60 {
            now = start + Duration::from_secs(step * 30);
            watchdog.observe(
                &raw_event(
                    "message_update",
                    json!({"assistantMessageEvent": {"type": "text_delta", "delta": ""}}),
                ),
                now,
            );
        }
        assert!(watchdog.expired(now).is_some());
    }

    #[test]
    fn thinking_deltas_are_progress() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        for step in 1..=40 {
            let now = start + Duration::from_secs(step * 60);
            watchdog.observe(
                &raw_event(
                    "message_update",
                    json!({
                        "assistantMessageEvent": {
                            "type": "thinking_delta",
                            "delta": format!("considering option {step}")
                        }
                    }),
                ),
                now,
            );
            assert_eq!(watchdog.expired(now), None, "step {step}");
        }
    }

    #[test]
    fn a_silent_compaction_gets_its_own_deadline() {
        // Under a generic model-idle bound this dies at ten minutes. Compaction
        // emits nothing by design, so it needs its own.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(&raw_event("compaction_start", json!({})), start);
        assert_eq!(watchdog.phase(), CodingProgressPhase::Compaction);
        assert_eq!(watchdog.expired(start + Duration::from_secs(700)), None);
        assert_eq!(watchdog.expired(start + Duration::from_secs(899)), None);
        assert!(watchdog.expired(start + Duration::from_secs(901)).is_some());
    }

    #[test]
    fn compaction_end_returns_to_the_model_phase() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(&raw_event("compaction_start", json!({})), start);
        let after = start + Duration::from_secs(300);
        watchdog.observe(&raw_event("compaction_end", json!({})), after);
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
        assert_eq!(watchdog.expired(after + Duration::from_secs(599)), None);
        assert!(watchdog.expired(after + Duration::from_secs(601)).is_some());
    }

    #[test]
    fn an_auto_retry_honours_its_declared_backoff() {
        // A nine-minute declared wait must not be read as nine minutes of
        // silence. The retry told us how long it would take.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("auto_retry_start", json!({"delayMs": 9 * 60 * 1000})),
            start,
        );
        assert_eq!(watchdog.phase(), CodingProgressPhase::RetryWait);
        // Declared 9 min + 2 min grace = 11 min.
        assert_eq!(watchdog.expired(start + Duration::from_secs(650)), None);
        assert!(watchdog.expired(start + Duration::from_secs(670)).is_some());
    }

    #[test]
    fn a_retry_with_no_declared_delay_still_gets_a_generous_bound() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(&raw_event("auto_retry_start", json!({})), start);
        // model_idle (600) + grace (120).
        assert_eq!(watchdog.expired(start + Duration::from_secs(700)), None);
        assert!(watchdog.expired(start + Duration::from_secs(730)).is_some());
    }

    #[test]
    fn a_nested_retry_delay_is_found() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("auto_retry_start", json!({"retry": {"delayMs": 300_000}})),
            start,
        );
        assert_eq!(watchdog.expired(start + Duration::from_secs(410)), None);
        assert!(watchdog.expired(start + Duration::from_secs(425)).is_some());
    }

    #[test]
    fn retry_backoff_is_excluded_from_active_work() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("auto_retry_start", json!({"delayMs": 300_000})),
            start,
        );
        let resumed = start + Duration::from_secs(300);
        watchdog.observe(&raw_event("auto_retry_end", json!({})), resumed);
        assert_eq!(watchdog.backoff_total(), Duration::from_secs(300));
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
    }

    #[test]
    fn the_summarization_family_gets_its_own_phase() {
        for raw_type in [
            "summarization_retry_scheduled",
            "summarization_retry_attempt_start",
        ] {
            let start = Instant::now();
            let mut watchdog = ProgressWatchdog::new(budgets(), start);
            watchdog.observe(&raw_event(raw_type, json!({})), start);
            assert_eq!(
                watchdog.phase(),
                CodingProgressPhase::Summarization,
                "{raw_type}"
            );
            assert_eq!(watchdog.expired(start + Duration::from_secs(880)), None);
            assert!(watchdog.expired(start + Duration::from_secs(910)).is_some());
        }
    }

    #[test]
    fn a_summarization_retry_declaring_a_longer_wait_is_honoured() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event(
                "summarization_retry_scheduled",
                json!({"delayMs": 20 * 60 * 1000}),
            ),
            start,
        );
        // 20 min declared + 2 min grace beats the 15 min generic bound.
        assert_eq!(watchdog.expired(start + Duration::from_secs(1300)), None);
        assert!(watchdog
            .expired(start + Duration::from_secs(1330))
            .is_some());
    }

    #[test]
    fn summarization_retry_finished_returns_to_the_model_phase() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("summarization_retry_scheduled", json!({})),
            start,
        );
        let done = start + Duration::from_secs(60);
        watchdog.observe(&raw_event("summarization_retry_finished", json!({})), done);
        assert_eq!(watchdog.phase(), CodingProgressPhase::Model);
    }

    #[test]
    fn bash_execution_updates_are_treated_as_tool_output() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "bash"})),
            start,
        );
        for step in 1..=40 {
            let now = start + Duration::from_secs(step * 60);
            watchdog.observe(
                &raw_event(
                    "bash_execution_update",
                    json!({"output": format!("compiling crate {step}")}),
                ),
                now,
            );
            if now < start + Duration::from_secs(3000) {
                assert_eq!(watchdog.expired(now), None, "step {step}");
            }
        }
    }

    #[test]
    fn pure_notifications_never_extend_a_deadline() {
        for raw_type in [
            "queue_update",
            "session_info_changed",
            "thinking_level_changed",
            "extension_error",
            "definitely_not_a_pi_event",
        ] {
            let start = Instant::now();
            let mut watchdog = ProgressWatchdog::new(budgets(), start);
            let mut now = start;
            for step in 1..=60 {
                now = start + Duration::from_secs(step * 30);
                watchdog.observe(&raw_event(raw_type, json!({"n": step})), now);
            }
            assert!(
                watchdog.expired(now).is_some(),
                "{raw_type} must not hold the turn open"
            );
        }
    }

    #[test]
    fn the_kill_switch_disables_every_deadline() {
        let mut off = budgets();
        off.no_progress_enabled = false;
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(off, start);
        watchdog.observe(&raw_event("compaction_start", json!({})), start);
        assert_eq!(watchdog.deadline(), None);
        assert_eq!(watchdog.expired(start + Duration::from_secs(100_000)), None);
    }

    #[test]
    fn no_progress_names_the_phase_that_fired() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "cargo-test"})),
            start,
        );
        let reason = watchdog
            .expired(start + Duration::from_secs(1600))
            .expect("fires");
        match reason {
            CodingTerminationReason::NoProgress {
                phase,
                last_substantive_event,
                ..
            } => {
                assert_eq!(phase, CodingProgressPhase::Tool);
                let label = last_substantive_event.expect("carries the last real event");
                assert!(label.contains("cargo-test"), "{label}");
            },
            other => panic!("expected NoProgress, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_turn_timeout_means_no_wall_clock() {
        // The shape a long autonomous run wants: liveness and cost are the only
        // stops. Resolved to a year rather than threaded as an `Option`, since
        // nothing distinguishes the two.
        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 0;
        let resolved = ResolvedCodingBudgets::resolve(&settings, None);
        assert_eq!(
            resolved.turn_max,
            Duration::from_secs(UNBOUNDED_CODING_TURN_SECS)
        );
    }

    #[test]
    fn no_task_ceiling_is_the_default() {
        // Duration is not what decides whether a run should stop, so the
        // whole-task ceiling is opt-in. Active time is still measured.
        let resolved = ResolvedCodingBudgets::resolve(&MagicianCodingSettings::default(), None);
        assert_eq!(resolved.task_active_max, None);
        assert_eq!(resolved.coding_phase_max(), None);
        // With no ceiling the turn is never narrowed, however much has been spent.
        assert_eq!(
            resolved.turn_max_within(Duration::from_secs(999_999)),
            resolved.turn_max
        );
    }

    #[test]
    fn the_execution_ceiling_never_undercuts_the_turn() {
        // An execution runs several turns; bounding it below one turn would kill
        // work mid-turn for no reason.
        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 8 * 3600;
        settings.task_budget_secs = 3600;
        let resolved = ResolvedCodingBudgets::resolve(&settings, None);
        assert_eq!(
            resolved.execution_ceiling(),
            Some(Duration::from_secs(8 * 3600))
        );
    }

    #[test]
    fn an_unbounded_turn_does_not_widen_a_configured_task_ceiling() {
        // The turn floor exists so a ceiling below one turn cannot fail every
        // run on its first turn. Applied to an UNBOUNDED turn it would silently
        // widen a deliberate two-hour ceiling to a year, turning "no turn wall
        // clock" into "no task ceiling either".
        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 0;
        settings.task_budget_secs = 7200;
        let resolved = ResolvedCodingBudgets::resolve(&settings, None);
        assert_eq!(
            resolved.task_active_max,
            Some(Duration::from_secs(7200)),
            "an unbounded turn must not raise the task ceiling"
        );
        assert_eq!(
            resolved.turn_max,
            Duration::from_secs(UNBOUNDED_CODING_TURN_SECS)
        );
        // And the turn is still narrowed to what the ceiling leaves.
        assert_eq!(
            resolved.turn_max_within(Duration::ZERO),
            Duration::from_secs(6000)
        );
    }

    #[test]
    fn a_bounded_turn_still_floors_the_task_ceiling() {
        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 3600;
        settings.task_budget_secs = 60;
        let resolved = ResolvedCodingBudgets::resolve(&settings, None);
        assert_eq!(resolved.task_active_max, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn the_unbounded_turn_stays_inside_the_timer_runtimes_range() {
        // tokio's timer tops out around 2.2 years; sitting near that ceiling is
        // how a "never" turns into a panic or a silently-capped deadline.
        assert!(UNBOUNDED_CODING_TURN_SECS < 2 * 365 * 24 * 60 * 60);
    }

    #[test]
    fn the_reported_reserve_is_the_one_actually_applied() {
        let mut budgets = budgets();
        // Wider than the whole ceiling, so it is ignored — reporting 99_999
        // would claim time was held back that never was.
        budgets.verification_reserve = Duration::from_secs(99_999);
        let payload = budgets.telemetry(Duration::ZERO);
        assert_eq!(payload["verification_reserve_secs"], 0);
        assert_eq!(payload["coding_phase_max_secs"], 7200);
    }

    #[test]
    fn an_unbounded_turn_arms_no_execution_watchdog_either() {
        // A single turn may legitimately outlast any execution clock, so there
        // is nothing sane to bound the execution by.
        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 0;
        settings.task_budget_secs = 7200;
        let resolved = ResolvedCodingBudgets::resolve(&settings, None);
        assert_eq!(resolved.execution_ceiling(), None);
        // The task ceiling still governs — enforced in the handler, not here.
        assert_eq!(resolved.task_active_max, Some(Duration::from_secs(7200)));
    }

    #[test]
    fn no_task_ceiling_arms_no_execution_watchdog() {
        // `None`, not "a year": an execution-level clock adds nothing over the
        // per-turn bounds, and a year-long watchdog is a task nobody retires.
        let mut settings = MagicianCodingSettings::default();
        settings.task_budget_secs = 0;
        let uncapped = ResolvedCodingBudgets::resolve(&settings, None);
        assert_eq!(uncapped.execution_ceiling(), None);
    }

    #[test]
    fn a_zero_turn_timeout_is_not_clamped_to_one_second() {
        // `clamp(1, ..)` on a turn value turns "no wall clock" into a
        // ONE-SECOND turn — every run dead on its first await.
        assert_eq!(clamp_turn_timeout_secs(0), 0);
        assert_eq!(clamp_turn_timeout_secs(900), 900);
        assert_eq!(
            clamp_turn_timeout_secs(u64::MAX),
            MAX_CODING_TURN_TIMEOUT_SECS
        );
    }

    #[test]
    fn a_requested_turn_override_wins_over_the_configured_default() {
        let settings = MagicianCodingSettings::default();
        let resolved = ResolvedCodingBudgets::resolve(&settings, Some(900));
        assert_eq!(resolved.turn_max, Duration::from_secs(900));
    }

    #[test]
    fn a_requested_turn_cannot_widen_the_task_budget() {
        // Otherwise any caller could buy itself more whole-task budget just by
        // asking for a longer turn.
        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 600;
        settings.task_budget_secs = 1200;
        let resolved = ResolvedCodingBudgets::resolve(&settings, Some(7200));
        assert_eq!(resolved.task_active_max, Some(Duration::from_secs(1200)));
    }

    #[test]
    fn nested_retry_waits_do_not_lose_earlier_backoff() {
        // A second `auto_retry_start` before the first ended used to reset the
        // phase clock and silently discard the wait already served.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("auto_retry_start", json!({"delayMs": 60_000})),
            start,
        );
        let second = start + Duration::from_secs(60);
        watchdog.observe(
            &raw_event("auto_retry_start", json!({"delayMs": 60_000})),
            second,
        );
        let done = second + Duration::from_secs(60);
        watchdog.observe(&raw_event("auto_retry_end", json!({})), done);
        assert_eq!(watchdog.backoff_total(), Duration::from_secs(120));
    }

    #[test]
    fn a_completed_retry_charges_its_backoff_exactly_once() {
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        watchdog.observe(
            &raw_event("auto_retry_start", json!({"delayMs": 120_000})),
            start,
        );
        watchdog.observe(
            &raw_event("auto_retry_end", json!({})),
            start + Duration::from_secs(120),
        );
        assert_eq!(watchdog.backoff_total(), Duration::from_secs(120));
    }

    #[test]
    fn the_task_budget_is_never_smaller_than_one_turn() {
        // A task budget below the turn budget would make every run fail on its
        // first turn, which reads as a coding failure rather than a config one.
        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 3600;
        settings.task_budget_secs = 60;
        let resolved = ResolvedCodingBudgets::resolve(&settings, None);
        assert_eq!(resolved.task_active_max, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn the_verification_reserve_is_held_back_from_the_coding_phase() {
        let budgets = budgets();
        assert_eq!(budgets.coding_phase_max(), Some(Duration::from_secs(6000)));
        assert_eq!(
            budgets.turn_max_within(Duration::from_secs(5400)),
            Duration::from_secs(600)
        );
        assert_eq!(
            budgets.turn_max_within(Duration::from_secs(6100)),
            Duration::ZERO
        );
    }

    #[test]
    fn a_reserve_wider_than_the_ceiling_does_not_make_runs_unrunnable() {
        let mut budgets = budgets();
        budgets.verification_reserve = Duration::from_secs(99_999);
        assert_eq!(budgets.coding_phase_max(), budgets.task_active_max);
    }

    #[test]
    fn a_reserve_is_inert_without_a_ceiling() {
        // With nothing to be starved of, holding time back is meaningless.
        let mut budgets = budgets();
        budgets.task_active_max = None;
        budgets.verification_reserve = Duration::from_secs(1200);
        assert_eq!(budgets.coding_phase_max(), None);
        let payload = budgets.telemetry(Duration::from_secs(4 * 3600));
        assert_eq!(payload["task_active_spent_secs"], 14_400);
        // Absent, NOT zero: "no ceiling" must never read as "out of budget".
        assert!(payload["task_active_remaining_secs"].is_null());
        assert!(payload["verification_reserve_secs"].is_null());
    }

    #[test]
    fn the_check_interval_tracks_the_tightest_bound_and_stays_coarse() {
        // The detector is polled on a cadence rather than at an exact deadline,
        // so its cost is independent of the ~10k events a run streams.
        let mut budgets = budgets();
        assert_eq!(budgets.check_interval(), MAX_PROGRESS_CHECK_INTERVAL);

        budgets.model_idle = Duration::from_secs(40);
        assert_eq!(budgets.check_interval(), Duration::from_secs(10));

        // A zero-length retry grace must not drive the tick to its floor: it is
        // an additive term, not a bound of its own.
        budgets.retry_grace = Duration::ZERO;
        assert_eq!(budgets.check_interval(), Duration::from_secs(10));

        budgets.model_idle = Duration::from_millis(1);
        assert_eq!(budgets.check_interval(), MIN_PROGRESS_CHECK_INTERVAL);
    }

    #[test]
    fn a_huge_payload_costs_the_same_to_fingerprint_as_a_small_one() {
        // The node budget bounds how many values are visited, not how long each
        // is. Without a length bound, one multi-megabyte tool result would be
        // hashed in full on every streaming event.
        let short = raw_event(
            "tool_execution_update",
            json!({ "toolName": "bash", "output": "x".repeat(16) }),
        );
        let long = raw_event(
            "tool_execution_update",
            json!({ "toolName": "bash", "output": "x".repeat(256 * 1024) }),
        );
        // Different lengths still fingerprint differently…
        assert_ne!(fingerprint_event(&short), fingerprint_event(&long));
        // …and an identical huge payload still matches itself, which is what
        // makes the repeat rule work on large tool output.
        let long_again = raw_event(
            "tool_execution_update",
            json!({ "toolName": "bash", "output": "x".repeat(256 * 1024) }),
        );
        assert_eq!(fingerprint_event(&long), fingerprint_event(&long_again));
    }

    #[test]
    fn two_large_payloads_differing_late_are_treated_as_repeats() {
        // The honest cost of the prefix bound, stated as a test rather than
        // left to be discovered: two payloads identical for their first 128
        // bytes AND the same length hash alike. For a stall detector that is
        // the right trade — it can only ever delay a kill to the next real
        // difference, never kill healthy work.
        let head = "y".repeat(FINGERPRINT_STRING_BYTES);
        let first = raw_event(
            "tool_execution_update",
            json!({ "output": format!("{head}aaaa") }),
        );
        let second = raw_event(
            "tool_execution_update",
            json!({ "output": format!("{head}bbbb") }),
        );
        assert_eq!(fingerprint_event(&first), fingerprint_event(&second));
    }

    #[test]
    fn the_last_substantive_label_is_built_only_on_failure() {
        // The hot path stores a static kind name and a tool name that is only
        // re-allocated when it changes; the string is assembled once, here.
        let start = Instant::now();
        let mut watchdog = ProgressWatchdog::new(budgets(), start);
        assert_eq!(watchdog.last_substantive_label(), None);
        watchdog.observe(
            &raw_event("tool_execution_start", json!({"toolName": "cargo-test"})),
            start,
        );
        assert_eq!(
            watchdog.last_substantive_label().as_deref(),
            Some("tool_execution_start (cargo-test)")
        );
        watchdog.observe(&raw_event("turn_end", json!({})), start);
        assert_eq!(
            watchdog.last_substantive_label().as_deref(),
            Some("turn_end")
        );
    }

    #[test]
    fn telemetry_reports_spend_and_remaining() {
        let payload = budgets().telemetry(Duration::from_secs(1200));
        assert_eq!(payload["task_active_spent_secs"], 1200);
        assert_eq!(payload["task_active_remaining_secs"], 4800);
        assert_eq!(payload["coding_phase_max_secs"], 6000);
    }

    #[test]
    fn the_termination_registry_keeps_the_first_writer() {
        let execution = format!("exec-{}", uuid::Uuid::new_v4());
        assert!(record_termination_reason(
            &execution,
            CodingTerminationReason::NoProgress {
                phase: CodingProgressPhase::Tool,
                elapsed_secs: 1500,
                last_substantive_event: None,
            }
        ));
        // A later, more generic cause must not overwrite the specific one that
        // actually fired.
        assert!(!record_termination_reason(
            &execution,
            CodingTerminationReason::ParentDeadline
        ));
        assert!(matches!(
            termination_reason(&execution),
            Some(CodingTerminationReason::NoProgress { .. })
        ));
        clear_termination_reason(&execution);
        assert_eq!(termination_reason(&execution), None);
    }

    #[test]
    fn an_empty_execution_id_is_never_registered() {
        assert!(!record_termination_reason(
            "",
            CodingTerminationReason::OwnerCancelled
        ));
        assert_eq!(termination_reason(""), None);
    }

    #[test]
    fn budget_stops_are_distinguishable_from_failures() {
        assert!(CodingTerminationReason::TurnTimeout { limit_secs: 60 }.is_budget_stop());
        assert!(CodingTerminationReason::TaskBudget {
            limit_secs: 60,
            active_secs: 61
        }
        .is_budget_stop());
        assert!(CodingTerminationReason::NoProgress {
            phase: CodingProgressPhase::Model,
            elapsed_secs: 1,
            last_substantive_event: None,
        }
        .is_budget_stop());
        assert!(!CodingTerminationReason::OwnerCancelled.is_budget_stop());
        assert!(!CodingTerminationReason::ParentDeadline.is_budget_stop());
    }

    #[test]
    fn termination_reasons_round_trip_through_json() {
        let reason = CodingTerminationReason::NoProgress {
            phase: CodingProgressPhase::Compaction,
            elapsed_secs: 900,
            last_substantive_event: Some("compaction_start".to_string()),
        };
        let encoded = serde_json::to_value(&reason).expect("serialize");
        assert_eq!(encoded["cause"], "no_progress");
        assert_eq!(encoded["phase"], "compaction");
        let decoded: CodingTerminationReason =
            serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, reason);
    }
}
