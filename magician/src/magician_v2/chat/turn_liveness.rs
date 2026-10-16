//! Chat-turn liveness: a turn is stopped for not progressing, never for
//! being long.
//!
//! A turn that keeps working — model responses arriving, tool calls starting
//! and finishing — may run as long as it needs; its tool-call cap and cost
//! budget bound it. Two shapes are not progress and end the turn:
//!
//! - **Stalled**: nothing happened for `harness_turn_stall_seconds` while no
//!   work was in flight. A model or tool call that is still running pauses
//!   the clock — those calls carry their own timeouts, and governed runs
//!   emit no heartbeat that could tell a slow call from a hung one — so the
//!   stall stop catches a turn wedged *between* steps (a lock, a storage
//!   await, a lost wakeup), not a long step.
//! - **Spinning**: the same tool called with the same arguments failed the
//!   same way three times running. Events keep flowing, but nothing changes.
//!   The third failure carries a corrective note; a fourth ends the turn.
//!
//! An optional wall-clock backstop (`harness_turn_max_seconds`) is honoured
//! by callers that already enforced one; it is not paused by work in flight.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Why the watchdog ended a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStopCause {
    /// No progress for the stall bound with no work in flight.
    Stalled,
    /// The wall-clock backstop elapsed.
    WallClockExceeded,
}

impl TurnStopCause {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Stalled => "turn_stalled",
            Self::WallClockExceeded => "turn_wall_clock_exceeded",
        }
    }
}

/// Shared progress marker for one turn. Cheap to bump from any task.
#[derive(Debug)]
pub struct TurnProgress {
    origin: Instant,
    last_progress_ms: AtomicU64,
    in_flight: AtomicUsize,
}

impl TurnProgress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            last_progress_ms: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Record that the turn did something observable.
    pub fn bump(&self) {
        self.last_progress_ms
            .store(self.now_ms(), Ordering::Relaxed);
    }

    /// Mark one model or tool call in flight until the guard drops. Both the
    /// start and the end count as progress.
    pub fn begin_work(self: &Arc<Self>) -> WorkInFlight {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.bump();
        WorkInFlight {
            progress: Arc::clone(self),
        }
    }

    fn idle_for(&self) -> Duration {
        Duration::from_millis(
            self.now_ms()
                .saturating_sub(self.last_progress_ms.load(Ordering::Relaxed)),
        )
    }

    fn work_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::Acquire) > 0
    }

    fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// RAII marker for a model or tool call in flight.
#[derive(Debug)]
pub struct WorkInFlight {
    progress: Arc<TurnProgress>,
}

impl Drop for WorkInFlight {
    fn drop(&mut self) {
        self.progress.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.progress.bump();
    }
}

/// The decision one watchdog tick makes. Pure, so the rule is testable
/// without a clock.
fn evaluate(
    elapsed: Duration,
    idle_for: Duration,
    work_in_flight: bool,
    stall_after: Option<Duration>,
    wall_clock: Option<Duration>,
) -> Option<TurnStopCause> {
    if wall_clock.is_some_and(|limit| elapsed >= limit) {
        return Some(TurnStopCause::WallClockExceeded);
    }
    if !work_in_flight && stall_after.is_some_and(|limit| idle_for >= limit) {
        return Some(TurnStopCause::Stalled);
    }
    None
}

/// Cancels a turn that stalls or outlives its wall clock. Dropping it stops
/// watching; the cause it fired with stays readable for the turn's ending.
#[derive(Debug)]
pub struct TurnWatchdog {
    task: Option<tokio::task::JoinHandle<()>>,
    cause: Arc<OnceLock<TurnStopCause>>,
}

impl TurnWatchdog {
    /// `stall_seconds` / `wall_clock_seconds` of 0 disable that bound; with
    /// both 0 nothing is spawned.
    pub fn spawn(
        progress: Arc<TurnProgress>,
        stall_seconds: u64,
        wall_clock_seconds: u64,
        cancel: CancellationToken,
    ) -> Self {
        let cause = Arc::new(OnceLock::new());
        let stall_after = (stall_seconds > 0).then(|| Duration::from_secs(stall_seconds));
        let wall_clock = (wall_clock_seconds > 0).then(|| Duration::from_secs(wall_clock_seconds));
        if stall_after.is_none() && wall_clock.is_none() {
            return Self { task: None, cause };
        }
        let tick = [stall_after, wall_clock]
            .into_iter()
            .flatten()
            .map(|bound| bound / 4)
            .min()
            .unwrap_or(Duration::from_secs(5))
            .clamp(Duration::from_millis(50), Duration::from_secs(5));
        let fired = Arc::clone(&cause);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(tick) => {},
                }
                if let Some(stop) = evaluate(
                    progress.elapsed(),
                    progress.idle_for(),
                    progress.work_in_flight(),
                    stall_after,
                    wall_clock,
                ) {
                    let _ = fired.set(stop);
                    cancel.cancel();
                    return;
                }
            }
        });
        Self {
            task: Some(task),
            cause,
        }
    }

    /// Why the watchdog cancelled the turn, if it did.
    pub fn cause(&self) -> Option<TurnStopCause> {
        self.cause.get().copied()
    }
}

impl Drop for TurnWatchdog {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The user-facing line for a turn the watchdog ended.
pub fn turn_stop_message(
    cause: TurnStopCause,
    stall_seconds: u64,
    wall_clock_seconds: u64,
) -> String {
    match cause {
        TurnStopCause::Stalled => format!(
            "I stopped this turn: it made no progress for {stall_seconds} seconds with no \
             model or tool call running. Send a message to continue."
        ),
        TurnStopCause::WallClockExceeded => format!(
            "I stopped this turn: it reached its {wall_clock_seconds}-second time limit. Send a \
             message to continue."
        ),
    }
}

/// Consecutive identical outcomes that earn a corrective note; one more ends
/// the turn.
pub const SPIN_NOTE_AFTER: u32 = 3;

/// One dispatched call's outcome, as the spin detector sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinOutcome<'a> {
    /// Failed, with its failure class (an error code or status).
    Failed(&'a str),
    /// Succeeded, with a fingerprint of its result. The same call returning
    /// the same result again changed nothing either: on 2026-10-11 a chat
    /// turn re-ran one `tool_search select` ~150 times, each "ok" and each
    /// identical, until it timed out, because only failures counted.
    Succeeded(&'a str),
}

/// What a turn should do after observing one tool outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpinVerdict {
    /// Not spinning.
    Continue,
    /// The [`SPIN_NOTE_AFTER`]th identical failure: tell the model.
    Note(String),
    /// The model repeated the call after the note: end the turn.
    Stop(String),
}

/// Tracks the current run of identical calls with identical outcomes in one
/// turn. The identity is (tool, canonical arguments fingerprint, outcome) —
/// the same arguments fingerprint tool lineage records — so a different
/// argument, a different error, a different result, or a failure after
/// successes (or the reverse) breaks the run.
#[derive(Debug, Default)]
pub struct SpinDetector {
    streak: Option<(String, String, String)>,
    count: u32,
}

impl SpinDetector {
    /// Observe one dispatched call. `failure` is the failure class (an error
    /// code or status) when the call failed, `None` when it succeeded.
    pub fn observe(
        &mut self,
        tool_name: &str,
        arguments_fingerprint: &str,
        outcome: SpinOutcome<'_>,
    ) -> SpinVerdict {
        let (class, failed) = match outcome {
            SpinOutcome::Failed(failure) => (format!("failed:{failure}"), true),
            SpinOutcome::Succeeded(result) => (format!("ok:{result}"), false),
        };
        let key = (
            tool_name.to_string(),
            arguments_fingerprint.to_string(),
            class,
        );
        if self.streak.as_ref() == Some(&key) {
            self.count = self.count.saturating_add(1);
        } else {
            self.streak = Some(key);
            self.count = 1;
        }
        if self.count == SPIN_NOTE_AFTER {
            SpinVerdict::Note(if failed {
                format!(
                    "This exact `{tool_name}` call has now failed {SPIN_NOTE_AFTER} times in a row \
                     with the same arguments and the same error. Repeating it will not succeed: \
                     change the arguments, use a different tool, or tell the user what is blocking \
                     you."
                )
            } else {
                format!(
                    "This exact `{tool_name}` call has now returned the same result \
                     {SPIN_NOTE_AFTER} times in a row. Repeating it will not change anything: use \
                     that result, try something different, or tell the user what is blocking you."
                )
            })
        } else if self.count > SPIN_NOTE_AFTER {
            SpinVerdict::Stop(if failed {
                format!(
                    "I stopped this turn: the same `{tool_name}` call kept failing with the same \
                     error ({} identical attempts), so repeating it was not making progress.",
                    self.count
                )
            } else {
                format!(
                    "I stopped this turn: the same `{tool_name}` call kept returning the same \
                     result ({} identical calls), so repeating it was not making progress.",
                    self.count
                )
            })
        } else {
            SpinVerdict::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_stalls_only_when_idle_with_nothing_in_flight() {
        let stall = Some(Duration::from_secs(300));
        let idle = Duration::from_secs(301);
        assert_eq!(
            evaluate(Duration::from_secs(400), idle, false, stall, None),
            Some(TurnStopCause::Stalled)
        );
        // A long model/tool call in flight is not a stall.
        assert_eq!(
            evaluate(Duration::from_secs(400), idle, true, stall, None),
            None
        );
        // Recent progress is not a stall, however long the turn.
        assert_eq!(
            evaluate(
                Duration::from_secs(36_000),
                Duration::from_secs(10),
                false,
                stall,
                None
            ),
            None
        );
        // Disabled.
        assert_eq!(
            evaluate(Duration::from_secs(400), idle, false, None, None),
            None
        );
    }

    #[test]
    fn evaluate_wall_clock_backstop_ignores_work_in_flight() {
        let wall = Some(Duration::from_secs(60));
        assert_eq!(
            evaluate(Duration::from_secs(60), Duration::ZERO, true, None, wall),
            Some(TurnStopCause::WallClockExceeded)
        );
        assert_eq!(
            evaluate(Duration::from_secs(59), Duration::ZERO, true, None, wall),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_that_keeps_progressing_is_never_stopped() {
        let progress = TurnProgress::new();
        let cancel = CancellationToken::new();
        let watchdog = TurnWatchdog::spawn(Arc::clone(&progress), 10, 0, cancel.clone());
        // 60 seconds of activity every 5 seconds: six stall bounds long.
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            progress.bump();
        }
        assert!(!cancel.is_cancelled());
        assert_eq!(watchdog.cause(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_turn_with_nothing_in_flight_is_stopped_as_stalled() {
        let progress = TurnProgress::new();
        progress.bump();
        let cancel = CancellationToken::new();
        let watchdog = TurnWatchdog::spawn(Arc::clone(&progress), 10, 0, cancel.clone());
        tokio::time::sleep(Duration::from_secs(13)).await;
        assert!(cancel.is_cancelled());
        assert_eq!(watchdog.cause(), Some(TurnStopCause::Stalled));
    }

    #[tokio::test(start_paused = true)]
    async fn work_in_flight_pauses_the_stall_clock() {
        let progress = TurnProgress::new();
        let cancel = CancellationToken::new();
        let watchdog = TurnWatchdog::spawn(Arc::clone(&progress), 10, 0, cancel.clone());
        let call = progress.begin_work();
        // A silent tool call five stall bounds long.
        tokio::time::sleep(Duration::from_secs(50)).await;
        assert!(!cancel.is_cancelled(), "a running call is not a stall");
        drop(call);
        // Its end is progress; silence after it is a stall again.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!cancel.is_cancelled());
        tokio::time::sleep(Duration::from_secs(8)).await;
        assert!(cancel.is_cancelled());
        assert_eq!(watchdog.cause(), Some(TurnStopCause::Stalled));
    }

    #[tokio::test(start_paused = true)]
    async fn the_wall_clock_backstop_fires_even_while_work_is_in_flight() {
        let progress = TurnProgress::new();
        let cancel = CancellationToken::new();
        let watchdog = TurnWatchdog::spawn(Arc::clone(&progress), 10, 30, cancel.clone());
        let _call = progress.begin_work();
        tokio::time::sleep(Duration::from_secs(32)).await;
        assert!(cancel.is_cancelled());
        assert_eq!(watchdog.cause(), Some(TurnStopCause::WallClockExceeded));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_bounds_disable_the_watchdog() {
        let progress = TurnProgress::new();
        let cancel = CancellationToken::new();
        let watchdog = TurnWatchdog::spawn(progress, 0, 0, cancel.clone());
        tokio::time::sleep(Duration::from_secs(3_600)).await;
        assert!(!cancel.is_cancelled());
        assert_eq!(watchdog.cause(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_watchdog_stops_it() {
        let progress = TurnProgress::new();
        let cancel = CancellationToken::new();
        drop(TurnWatchdog::spawn(progress, 10, 0, cancel.clone()));
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn the_third_identical_failure_earns_a_note_and_the_fourth_stops() {
        let mut spin = SpinDetector::default();
        assert_eq!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("not_found")),
            SpinVerdict::Continue
        );
        assert_eq!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("not_found")),
            SpinVerdict::Continue
        );
        assert!(matches!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("not_found")),
            SpinVerdict::Note(note) if note.contains("3 times")
        ));
        assert!(matches!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("not_found")),
            SpinVerdict::Stop(_)
        ));
    }

    /// The 2026-10-11 loop: the same select, "ok" every time, identical result.
    #[test]
    fn the_same_successful_result_repeated_is_a_spin_too() {
        let mut spin = SpinDetector::default();
        let same = SpinOutcome::Succeeded("loaded:agentmail-send");
        assert_eq!(
            spin.observe("tool_search", "fp", same),
            SpinVerdict::Continue
        );
        assert_eq!(
            spin.observe("tool_search", "fp", same),
            SpinVerdict::Continue
        );
        assert!(matches!(
            spin.observe("tool_search", "fp", same),
            SpinVerdict::Note(note) if note.contains("same result")
        ));
        assert!(matches!(
            spin.observe("tool_search", "fp", same),
            SpinVerdict::Stop(text) if text.contains("same result")
        ));
        // A result that moves (a status being polled) is progress.
        let mut polling = SpinDetector::default();
        for status in ["queued", "running", "running:2", "running:3", "done"] {
            assert_eq!(
                polling.observe("task_status", "fp", SpinOutcome::Succeeded(status)),
                SpinVerdict::Continue
            );
        }
    }

    #[test]
    fn a_success_or_any_different_call_resets_the_streak() {
        let mut spin = SpinDetector::default();
        spin.observe("read_file", "fp", SpinOutcome::Failed("not_found"));
        spin.observe("read_file", "fp", SpinOutcome::Failed("not_found"));
        // Different arguments.
        assert_eq!(
            spin.observe("read_file", "fp2", SpinOutcome::Failed("not_found")),
            SpinVerdict::Continue
        );
        spin.observe("read_file", "fp", SpinOutcome::Failed("not_found"));
        spin.observe("read_file", "fp", SpinOutcome::Failed("not_found"));
        // Different error.
        assert_eq!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("denied")),
            SpinVerdict::Continue
        );
        spin.observe("read_file", "fp", SpinOutcome::Failed("not_found"));
        spin.observe("read_file", "fp", SpinOutcome::Failed("not_found"));
        // A success.
        assert_eq!(
            spin.observe("read_file", "fp", SpinOutcome::Succeeded("r1")),
            SpinVerdict::Continue
        );
        assert_eq!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("not_found")),
            SpinVerdict::Continue
        );
        assert_eq!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("not_found")),
            SpinVerdict::Continue
        );
        assert!(matches!(
            spin.observe("read_file", "fp", SpinOutcome::Failed("not_found")),
            SpinVerdict::Note(_)
        ));
    }
}
