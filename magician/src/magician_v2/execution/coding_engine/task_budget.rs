//! Durable active-time accounting for a whole coding task.
//!
//! The turn budget bounds one Pi turn. This bounds the task those turns belong
//! to — including verification and repair — and it has to survive a restart,
//! because a budget that resets when the process bounces is not a budget.
//!
//! Three things make this more than a stopwatch:
//!
//! - **Active time is the UNION of active intervals, not their sum.** Summing
//!   child durations would exhaust a two-hour budget in forty minutes of real
//!   time the moment any work runs in parallel. Cost genuinely does add up;
//!   time does not.
//! - **Waiting on a person is not spending.** An interval closes when the turn
//!   returns, so time parked on diff approval falls outside every interval.
//!   Charging it would punish review.
//! - **A crashed run must not charge forever.** An interval stamps the latest
//!   instant it could possibly have run to when it opens — its turn's wall
//!   clock, capped at [`MAX_SEAL_HORIZON`] because that wall clock may be
//!   configured unbounded — so a process that dies mid-turn is sealed rather
//!   than charging for all of time.
//!
//! Plan: `docs/archive/plans/2026-08-07-vibedev-run-duration.md` §6.1.

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::budgets::{CodingTerminationReason, ResolvedCodingBudgets};

const LEDGER_VERSION: u32 = 1;

/// Filename under the task directory. Task-level, not execution-level, because
/// the budget spans every turn, child and repair round of the same task.
pub const LEDGER_FILE_NAME: &str = "coding_budget_ledger.json";

/// One stretch of genuinely active coding work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveInterval {
    pub execution_id: String,
    pub start_ms: i64,
    /// `None` while the interval is still open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
    /// The latest instant this interval could possibly have run to, stamped
    /// when it opened. Without it, a crash leaves an open interval that charges
    /// against the budget for as long as the task exists.
    pub sealed_at_ms: i64,
    /// Time inside this interval that was declared provider backoff rather than
    /// work. A rate-limit wait is not the agent thinking.
    #[serde(default)]
    pub excluded_ms: u64,
}

impl ActiveInterval {
    /// The window this interval occupies, clamped so an open interval never
    /// runs past the bound it declared for itself.
    fn window(&self, now_ms: i64) -> (i64, i64) {
        let end = match self.end_ms {
            Some(end) => end,
            None => now_ms.min(self.sealed_at_ms),
        };
        (self.start_ms, end.max(self.start_ms))
    }

    fn is_open(&self) -> bool {
        self.end_ms.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerFile {
    #[serde(default = "ledger_version")]
    version: u32,
    #[serde(default)]
    intervals: Vec<ActiveInterval>,
    /// Union time already folded away by compaction. The file is rewritten on
    /// every open and close, so an unbounded interval list would make each turn
    /// progressively more expensive — and with no task ceiling (the default)
    /// nothing else bounds the turn count.
    #[serde(default)]
    compacted_ms: u64,
}

fn ledger_version() -> u32 {
    LEDGER_VERSION
}

/// Closed intervals are folded into `compacted_ms` past this count.
const MAX_TRACKED_INTERVALS: usize = 256;

/// Most an interval abandoned by a crash can ever charge.
///
/// The seal is normally the turn's own wall clock, but a turn may be configured
/// unbounded — and sealing at "a year" would let one crashed run report a year
/// of active time forever. There is no plausible turn longer than this, and
/// under-charging an abandoned interval is far safer than over-charging one.
const MAX_SEAL_HORIZON: Duration = Duration::from_secs(24 * 60 * 60);

impl Default for LedgerFile {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            intervals: Vec::new(),
            compacted_ms: 0,
        }
    }
}

impl LedgerFile {
    fn total_active(&self, now_ms: i64) -> Duration {
        Duration::from_millis(self.compacted_ms)
            .saturating_add(active_duration(&self.intervals, now_ms))
    }

    /// Fold away closed intervals that can no longer overlap anything live.
    ///
    /// Only intervals that ended at or before the earliest still-open start are
    /// eligible: folding one that overlaps a live interval would double-count
    /// the shared stretch, which is exactly the error the union exists to
    /// prevent.
    fn compact(&mut self, now_ms: i64) {
        if self.intervals.len() <= MAX_TRACKED_INTERVALS {
            return;
        }
        let boundary = self
            .intervals
            .iter()
            .filter(|interval| interval.is_open())
            .map(|interval| interval.start_ms)
            .min()
            .unwrap_or(i64::MAX);
        let (foldable, live): (Vec<_>, Vec<_>) = self.intervals.drain(..).partition(|interval| {
            !interval.is_open() && interval.end_ms.unwrap_or(i64::MIN) <= boundary
        });
        self.intervals = live;
        if foldable.is_empty() {
            return;
        }
        let folded = active_duration(&foldable, now_ms);
        self.compacted_ms = self
            .compacted_ms
            .saturating_add(folded.as_millis().min(u64::MAX as u128) as u64);
    }
}

/// How much active time a task has spent, and — only if a ceiling was
/// configured — how much of it is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskBudgetStatus {
    /// Active time spent so far, union-counted. Always meaningful.
    pub active: Duration,
    /// The slice of a task ceiling the coding phase may use, with the
    /// verification reserve held back. `None` when there is no ceiling.
    pub coding_phase_max: Option<Duration>,
    /// What is left of that slice. `None` when there is no ceiling — which is
    /// different from zero, and conflating the two is how "no budget
    /// configured" starts reading as "out of budget".
    pub remaining: Option<Duration>,
    /// The turn backstop, narrowed to what remains when a ceiling applies.
    pub turn_max: Duration,
}

impl TaskBudgetStatus {
    /// The termination cause to report when a configured ceiling stops a run.
    ///
    /// Reports the **coding phase** ceiling, not the whole task budget: the
    /// verification reserve is held back and was never available to spend, so
    /// naming the larger number would tell the operator they had room they
    /// never had.
    pub fn termination_reason(&self) -> CodingTerminationReason {
        CodingTerminationReason::TaskBudget {
            limit_secs: self
                .coding_phase_max
                .map(|max| max.as_secs())
                .unwrap_or_default(),
            active_secs: self.active.as_secs(),
        }
    }

    /// True when a configured ceiling leaves too little for a useful turn.
    ///
    /// Always false with no ceiling — the default. Duration alone never stops a
    /// run; the liveness detector and the cost ceiling do.
    pub fn blocks_new_work(&self) -> bool {
        self.remaining
            .is_some_and(|remaining| remaining < MIN_USEFUL_TURN)
    }
}

/// Serializes read-modify-write across every ledger in the process.
///
/// Two coding turns on the same task can run concurrently — that is the whole
/// reason active time is union-counted rather than summed. Each would otherwise
/// hold its own snapshot loaded at open time and write the entire file back,
/// so whichever finished last would silently erase the other's interval. The
/// budget would then under-count exactly when the most work was happening.
///
/// One global lock rather than one per path: writes happen twice per turn, and
/// a turn is minutes long.
fn ledger_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// The durable ledger for one task.
///
/// Deliberately holds no cached state. The file on disk is the single source of
/// truth, re-read for every query and every mutation, so a concurrent turn's
/// intervals can never be clobbered by a stale in-memory copy.
pub struct CodingTaskBudgetLedger {
    path: PathBuf,
}

impl CodingTaskBudgetLedger {
    pub fn open(task_dir: &Path) -> Self {
        Self {
            path: task_dir.join(LEDGER_FILE_NAME),
        }
    }

    /// Read the ledger. A file that cannot be read or parsed is treated as
    /// empty rather than as a hard failure: losing accounting is bad, but
    /// refusing to code because a JSON file is corrupt is worse.
    fn load(&self) -> LedgerFile {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return LedgerFile::default();
        };
        match serde_json::from_slice::<LedgerFile>(&bytes) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    target: "coding_engine",
                    path = %self.path.display(),
                    %error,
                    "coding budget ledger is unreadable; starting a fresh one"
                );
                LedgerFile::default()
            },
        }
    }

    /// Read-modify-write under the process-wide lock, so a concurrent turn's
    /// intervals survive.
    fn mutate(&self, apply: impl FnOnce(&mut LedgerFile) -> bool) {
        let _guard = match ledger_write_lock().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut state = self.load();
        if !apply(&mut state) {
            return;
        }
        if let Err(error) = write_atomic(&self.path, &state) {
            // Degrade to un-persisted accounting rather than failing the run.
            // The turn budget still bounds this run; only the whole-task total
            // stops surviving a restart, and saying so is more useful than a
            // dead coding task.
            tracing::warn!(
                target: "coding_engine",
                path = %self.path.display(),
                %error,
                "failed to persist the coding budget ledger; this turn is uncharged"
            );
        }
    }

    /// Begin charging active time for an execution.
    ///
    /// Re-opening an execution that is already open is a no-op, so a retried
    /// dispatch cannot double-charge the same stretch of work.
    pub fn open_interval(&self, execution_id: &str, now_ms: i64, turn_max: Duration) {
        let horizon = turn_max.min(MAX_SEAL_HORIZON);
        let sealed_at_ms = now_ms.saturating_add(horizon.as_millis().min(i64::MAX as u128) as i64);
        self.mutate(|state| {
            let already_open = state
                .intervals
                .iter()
                .any(|interval| interval.is_open() && interval.execution_id == execution_id);
            if already_open {
                return false;
            }
            state.intervals.push(ActiveInterval {
                execution_id: execution_id.to_string(),
                start_ms: now_ms,
                end_ms: None,
                sealed_at_ms,
                excluded_ms: 0,
            });
            state.compact(now_ms);
            true
        });
    }

    /// Stop charging, recording how much of the interval was declared backoff
    /// rather than work.
    pub fn close_interval(&self, execution_id: &str, now_ms: i64, excluded: Duration) {
        self.mutate(|state| {
            let Some(interval) = state
                .intervals
                .iter_mut()
                .rev()
                .find(|interval| interval.is_open() && interval.execution_id == execution_id)
            else {
                return false;
            };
            interval.end_ms = Some(now_ms.max(interval.start_ms));
            interval.excluded_ms = excluded.as_millis().min(u64::MAX as u128) as u64;
            true
        });
    }

    /// Union-counted active time, with declared backoff removed.
    pub fn active(&self, now_ms: i64) -> Duration {
        self.load().total_active(now_ms)
    }

    /// Where the task stands against its budget.
    pub fn status(&self, budgets: &ResolvedCodingBudgets, now_ms: i64) -> TaskBudgetStatus {
        status_for(budgets, self.active(now_ms))
    }
}

/// Below this, what remains of the task budget cannot buy a useful coding turn.
///
/// Handing Pi a five-second timeout spawns a process, pays the model's first
/// round trip and dies — reported as a coding failure. Refusing up front is
/// both cheaper and more honest.
pub const MIN_USEFUL_TURN: Duration = Duration::from_secs(60);

/// Budget arithmetic, separated from storage so it can be checked directly.
pub fn status_for(budgets: &ResolvedCodingBudgets, active: Duration) -> TaskBudgetStatus {
    let coding_phase_max = budgets.coding_phase_max();
    let remaining = coding_phase_max.map(|max| max.checked_sub(active).unwrap_or_default());
    TaskBudgetStatus {
        active,
        coding_phase_max,
        remaining,
        turn_max: budgets.turn_max_within(active),
    }
}

/// Union of the intervals, minus declared backoff.
///
/// The union is the point: two children running side by side occupy one stretch
/// of wall clock between them, and summing their durations would charge the
/// task twice for time it only spent once.
fn active_duration(intervals: &[ActiveInterval], now_ms: i64) -> Duration {
    let mut windows: Vec<(i64, i64)> = intervals
        .iter()
        .map(|interval| interval.window(now_ms))
        .filter(|(start, end)| end > start)
        .collect();
    windows.sort_unstable();

    let mut total_ms: i64 = 0;
    let mut current: Option<(i64, i64)> = None;
    for (start, end) in windows {
        match current {
            Some((open_start, open_end)) if start <= open_end => {
                current = Some((open_start, open_end.max(end)));
            },
            Some((open_start, open_end)) => {
                total_ms = total_ms.saturating_add(open_end - open_start);
                current = Some((start, end));
            },
            None => current = Some((start, end)),
        }
    }
    if let Some((open_start, open_end)) = current {
        total_ms = total_ms.saturating_add(open_end - open_start);
    }

    // Backoff is subtracted from the union total rather than from individual
    // windows: an exclusion inside an overlap cannot be attributed to one of
    // the overlapping intervals. Exact while work is sequential, which is the
    // only shape coding runs currently take, and conservative otherwise.
    let excluded_ms: i64 = intervals
        .iter()
        .map(|interval| i64::try_from(interval.excluded_ms).unwrap_or(i64::MAX))
        .fold(0i64, |acc, value| acc.saturating_add(value));

    Duration::from_millis(total_ms.saturating_sub(excluded_ms).max(0) as u64)
}

fn write_atomic(path: &Path, snapshot: &LedgerFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let encoded = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    // Write beside the target and rename, so a crash mid-write leaves the
    // previous ledger intact instead of a truncated one. The temp name is
    // unique per write: a shared one would let two writers interleave into the
    // same file and then rename the mixture into place, which is worse than the
    // torn write the rename exists to prevent.
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result =
        std::fs::write(&temporary, encoded).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;

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

    fn interval(execution_id: &str, start_ms: i64, end_ms: Option<i64>) -> ActiveInterval {
        ActiveInterval {
            execution_id: execution_id.to_string(),
            start_ms,
            end_ms,
            sealed_at_ms: start_ms + 3_600_000,
            excluded_ms: 0,
        }
    }

    #[test]
    fn sequential_intervals_add_up() {
        let intervals = vec![
            interval("a", 0, Some(60_000)),
            interval("b", 120_000, Some(180_000)),
        ];
        assert_eq!(
            active_duration(&intervals, 200_000),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn parallel_children_do_not_double_charge_wall_time() {
        // The failure this exists to prevent: two children running side by side
        // for forty minutes must charge forty minutes, not eighty.
        let intervals = vec![
            interval("a", 0, Some(2_400_000)),
            interval("b", 0, Some(2_400_000)),
        ];
        assert_eq!(
            active_duration(&intervals, 2_400_000),
            Duration::from_secs(2400)
        );
    }

    #[test]
    fn partially_overlapping_intervals_merge() {
        let intervals = vec![
            interval("a", 0, Some(100_000)),
            interval("b", 50_000, Some(150_000)),
        ];
        assert_eq!(
            active_duration(&intervals, 200_000),
            Duration::from_secs(150)
        );
    }

    #[test]
    fn a_gap_between_intervals_is_not_charged() {
        // The gap is diff approval. Charging it would punish review.
        let intervals = vec![
            interval("a", 0, Some(60_000)),
            interval("b", 3_660_000, Some(3_720_000)),
        ];
        assert_eq!(
            active_duration(&intervals, 3_720_000),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn an_open_interval_is_charged_up_to_now() {
        let intervals = vec![interval("a", 0, None)];
        assert_eq!(active_duration(&intervals, 90_000), Duration::from_secs(90));
    }

    #[test]
    fn an_abandoned_interval_is_capped_even_with_an_unbounded_turn() {
        // The seal is normally the turn's own wall clock — but a turn may be
        // configured unbounded, and sealing at "a year" would let one crashed
        // run report a year of active time forever.
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        ledger.open_interval("exec-1", 0, Duration::from_secs(365 * 24 * 3600));
        let a_year_later: i64 = 365 * 24 * 3_600_000;
        assert_eq!(ledger.active(a_year_later), MAX_SEAL_HORIZON);
    }

    #[test]
    fn an_abandoned_interval_is_sealed_at_its_declared_bound() {
        // The process died mid-turn. Without the seal this charges forever.
        let mut abandoned = interval("a", 0, None);
        abandoned.sealed_at_ms = 3_600_000;
        let a_week_later = 7 * 24 * 3_600_000;
        assert_eq!(
            active_duration(&[abandoned], a_week_later),
            Duration::from_secs(3600)
        );
    }

    #[test]
    fn declared_backoff_is_not_charged_as_work() {
        let mut with_backoff = interval("a", 0, Some(600_000));
        with_backoff.excluded_ms = 300_000;
        assert_eq!(
            active_duration(&[with_backoff], 600_000),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn backoff_wider_than_the_interval_floors_at_zero() {
        let mut odd = interval("a", 0, Some(60_000));
        odd.excluded_ms = 600_000;
        assert_eq!(active_duration(&[odd], 60_000), Duration::ZERO);
    }

    #[test]
    fn an_empty_ledger_has_spent_nothing() {
        assert_eq!(active_duration(&[], 1_000_000), Duration::ZERO);
    }

    #[test]
    fn a_zero_length_interval_is_ignored() {
        assert_eq!(
            active_duration(&[interval("a", 5_000, Some(5_000))], 10_000),
            Duration::ZERO
        );
    }

    #[test]
    fn status_holds_back_the_verification_reserve() {
        let status = status_for(&budgets(), Duration::from_secs(1200));
        assert_eq!(status.coding_phase_max, Some(Duration::from_secs(6000)));
        assert_eq!(status.remaining, Some(Duration::from_secs(4800)));
        assert!(!status.blocks_new_work());
        assert_eq!(status.turn_max, Duration::from_secs(3600));
    }

    #[test]
    fn status_narrows_the_last_turn_to_what_remains() {
        let status = status_for(&budgets(), Duration::from_secs(5700));
        assert_eq!(status.remaining, Some(Duration::from_secs(300)));
        assert_eq!(status.turn_max, Duration::from_secs(300));
        assert!(!status.blocks_new_work());
    }

    #[test]
    fn without_a_ceiling_duration_never_stops_a_run() {
        // The default. However long a task has been working, spend alone must
        // not end it — liveness and cost do that.
        let mut uncapped = budgets();
        uncapped.task_active_max = None;
        let after_days = status_for(&uncapped, Duration::from_secs(3 * 24 * 3600));
        assert_eq!(after_days.active, Duration::from_secs(259_200));
        assert_eq!(after_days.remaining, None);
        assert!(!after_days.blocks_new_work());
        // And the turn is never narrowed, so Pi is never handed a doomed clock.
        assert_eq!(after_days.turn_max, uncapped.turn_max);
        assert!(!after_days.turn_max.is_zero());
    }

    #[test]
    fn a_configured_ceiling_does_stop_a_run() {
        let status = status_for(&budgets(), Duration::from_secs(9999));
        assert_eq!(status.remaining, Some(Duration::ZERO));
        assert!(status.blocks_new_work());
    }

    #[test]
    fn a_remainder_too_small_to_use_counts_as_exhausted() {
        // Handing Pi a five-second timeout spawns a process, pays one model
        // round trip and dies — reported as a coding failure.
        let budgets = budgets();
        let coding_max = budgets
            .coding_phase_max()
            .expect("this fixture has a ceiling");
        let status = status_for(&budgets, coding_max - Duration::from_secs(5));
        assert_eq!(status.remaining, Some(Duration::from_secs(5)));
        assert!(status.blocks_new_work(), "5s cannot buy a coding turn");

        // Just above the floor is still usable, and the turn is narrowed to it.
        let usable = status_for(&budgets, coding_max - MIN_USEFUL_TURN);
        assert!(!usable.blocks_new_work());
        assert_eq!(usable.turn_max, MIN_USEFUL_TURN);
    }

    #[test]
    fn a_concurrent_turn_does_not_erase_another_turns_interval() {
        // Two coding turns on one task is the case union-counting exists for.
        // A cached snapshot per ledger would make the last writer clobber the
        // first, under-counting exactly when the most work is happening.
        let dir = TempDir::new().unwrap();
        let first = CodingTaskBudgetLedger::open(dir.path());
        let second = CodingTaskBudgetLedger::open(dir.path());

        first.open_interval("exec-a", 0, Duration::from_secs(3600));
        second.open_interval("exec-b", 0, Duration::from_secs(3600));
        first.close_interval("exec-a", 60_000, Duration::ZERO);
        second.close_interval("exec-b", 120_000, Duration::ZERO);

        let reloaded = CodingTaskBudgetLedger::open(dir.path());
        let file = reloaded.load();
        assert_eq!(file.intervals.len(), 2, "both intervals survived");
        // Overlapping, so the union is the wall clock they share — 120s, not 180.
        assert_eq!(reloaded.active(120_000), Duration::from_secs(120));
    }

    #[test]
    fn a_ledger_query_sees_another_ledgers_writes() {
        let dir = TempDir::new().unwrap();
        let writer = CodingTaskBudgetLedger::open(dir.path());
        let reader = CodingTaskBudgetLedger::open(dir.path());
        assert_eq!(reader.active(60_000), Duration::ZERO);

        writer.open_interval("exec-a", 0, Duration::from_secs(3600));
        writer.close_interval("exec-a", 60_000, Duration::ZERO);
        assert_eq!(reader.active(60_000), Duration::from_secs(60));
    }

    #[test]
    fn an_exhausted_budget_reports_the_ceiling_it_actually_hit() {
        // 6000, not 7200: the 1200s verification reserve was never available to
        // the coding phase, so naming the full task budget would tell the
        // operator they had room they never had.
        let status = status_for(&budgets(), Duration::from_secs(9999));
        let reason = status.termination_reason();
        assert!(reason.is_budget_stop());
        assert!(
            matches!(
                reason,
                CodingTerminationReason::TaskBudget {
                    limit_secs: 6000,
                    active_secs: 9999
                }
            ),
            "{reason:?}"
        );
    }

    #[test]
    fn the_ledger_survives_a_reload() {
        // "A budget that resets on restart is not a budget."
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        ledger.open_interval("exec-1", 0, Duration::from_secs(3600));
        ledger.close_interval("exec-1", 600_000, Duration::ZERO);
        assert_eq!(ledger.active(600_000), Duration::from_secs(600));

        let reloaded = CodingTaskBudgetLedger::open(dir.path());
        assert_eq!(reloaded.active(600_000), Duration::from_secs(600));
    }

    #[test]
    fn a_second_turn_accumulates_onto_the_first() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        ledger.open_interval("exec-1", 0, Duration::from_secs(3600));
        ledger.close_interval("exec-1", 600_000, Duration::ZERO);

        let resumed = CodingTaskBudgetLedger::open(dir.path());
        resumed.open_interval("exec-2", 1_200_000, Duration::from_secs(3600));
        resumed.close_interval("exec-2", 1_500_000, Duration::ZERO);
        assert_eq!(resumed.active(1_500_000), Duration::from_secs(900));
    }

    #[test]
    fn reopening_a_live_execution_does_not_double_charge() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        ledger.open_interval("exec-1", 0, Duration::from_secs(3600));
        ledger.open_interval("exec-1", 0, Duration::from_secs(3600));
        assert_eq!(ledger.active(600_000), Duration::from_secs(600));
    }

    #[test]
    fn closing_an_unknown_execution_is_harmless() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        ledger.close_interval("never-opened", 60_000, Duration::ZERO);
        assert_eq!(ledger.active(60_000), Duration::ZERO);
    }

    #[test]
    fn a_corrupt_ledger_starts_fresh_rather_than_failing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(LEDGER_FILE_NAME), b"{ not json").unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        assert_eq!(ledger.active(600_000), Duration::ZERO);
        // And it recovers: the next write replaces the corrupt file.
        ledger.open_interval("exec-1", 0, Duration::from_secs(3600));
        ledger.close_interval("exec-1", 60_000, Duration::ZERO);
        let reloaded = CodingTaskBudgetLedger::open(dir.path());
        assert_eq!(reloaded.active(60_000), Duration::from_secs(60));
    }

    #[test]
    fn a_ledger_left_open_by_a_crash_is_sealed_on_reload() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        ledger.open_interval("exec-1", 0, Duration::from_secs(1800));
        drop(ledger); // process dies mid-turn

        let reloaded = CodingTaskBudgetLedger::open(dir.path());
        let a_week_later = 7 * 24 * 3_600_000;
        assert_eq!(reloaded.active(a_week_later), Duration::from_secs(1800));
    }

    #[test]
    fn a_long_lived_ledger_compacts_instead_of_growing_without_bound() {
        // With no task ceiling (the default) nothing else caps the turn count,
        // and the whole file is rewritten on every open and close.
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        let turns = MAX_TRACKED_INTERVALS + 40;
        for turn in 0..turns {
            let start = turn as i64 * 120_000;
            ledger.open_interval(&format!("exec-{turn}"), start, Duration::from_secs(3600));
            ledger.close_interval(&format!("exec-{turn}"), start + 60_000, Duration::ZERO);
        }
        let now = turns as i64 * 120_000;
        // Every turn charged 60s, and compaction must not change the total.
        assert_eq!(ledger.active(now), Duration::from_secs(60 * turns as u64));

        let reloaded = CodingTaskBudgetLedger::open(dir.path());
        assert_eq!(reloaded.active(now), Duration::from_secs(60 * turns as u64));
    }

    #[test]
    fn compaction_never_folds_an_interval_that_overlaps_live_work() {
        // Folding a closed interval that overlaps an open one would double-count
        // the shared stretch — the exact error the union exists to prevent.
        let mut file = LedgerFile::default();
        // One long-running execution spanning everything else.
        file.intervals.push(interval("long", 0, None));
        for index in 0..(MAX_TRACKED_INTERVALS + 10) {
            let start = index as i64 * 1_000;
            file.intervals.push(interval(
                &format!("short-{index}"),
                start,
                Some(start + 500),
            ));
        }
        let now = 10_000_000;
        let before = file.total_active(now);
        file.compact(now);
        assert_eq!(file.compacted_ms, 0, "nothing was eligible to fold");
        assert_eq!(file.total_active(now), before);
    }

    #[test]
    fn compaction_preserves_the_union_when_folded_intervals_overlap() {
        let mut file = LedgerFile::default();
        for index in 0..(MAX_TRACKED_INTERVALS + 10) {
            // Deliberately overlapping: 0-1500, 1000-2500, 2000-3500, …
            let start = index as i64 * 1_000;
            file.intervals
                .push(interval(&format!("e-{index}"), start, Some(start + 1_500)));
        }
        let now = 10_000_000;
        let before = file.total_active(now);
        file.compact(now);
        assert!(file.compacted_ms > 0, "the closed run should have folded");
        assert_eq!(file.total_active(now), before);
    }

    #[test]
    fn status_reads_through_the_ledger() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingTaskBudgetLedger::open(dir.path());
        ledger.open_interval("exec-1", 0, Duration::from_secs(3600));
        ledger.close_interval("exec-1", 5_700_000, Duration::ZERO);
        let status = ledger.status(&budgets(), 5_700_000);
        assert_eq!(status.active, Duration::from_secs(5700));
        assert_eq!(status.remaining, Some(Duration::from_secs(300)));
    }
}
