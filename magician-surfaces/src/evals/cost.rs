//! What a run cost, joined from the LLM ledger at read time.
//!
//! # Why unknown is a value and not a zero
//!
//! This module exists for one invariant: **a lane that spent money must never
//! display as free because a query failed.** Three situations look alike from a
//! distance and are not alike at all:
//!
//! | situation | answer |
//! | --- | --- |
//! | the ledger has rows for the run's task | [`CostValue::Known`] of their sum |
//! | the ledger has no rows — the run genuinely made no LLM calls | [`CostValue::Known`] of `0.0` |
//! | the ledger could not be asked, or answered with something unusable | [`CostValue::Unknown`] |
//! | the run has no [`EvalRun::task_id`], so spend cannot be attributed | [`CostValue::Unknown`] |
//!
//! "Made no calls" and "could not ask" are different answers. Collapsing them —
//! in either direction — is the defect this module exists to prevent, so
//! [`CostValue`] has no numeric representation of "unknown" for anything to
//! default to, and [`costs_for_runs`] has **no error type**: there is no path on
//! which a failure could be swallowed into a number, because a failure is not
//! reported as a number at all.
//!
//! # Why cost is joined rather than stored
//!
//! See [`super::run`]: the ledger is the single source of truth for money and it
//! reprices (a corrected rate, a late-arriving usage record). A figure copied
//! onto the run record would freeze, giving the same run two answers and no way
//! to reconcile them. A join that says "unknown" is a visible gap; a stale
//! number is not.
//!
//! # Why the ledger is a trait
//!
//! Two reasons, and the second is the load-bearing one. The obvious one is that
//! the invariant above is about failure, and failure has to be *tested* — a fake
//! can hand back rows, emptiness and an error where a DuckDB fixture cannot.
//! The other is that [`EvalCostLedger::cost_rows_for_tasks`] takes a **slice**
//! of task ids: the natural shape of this join is one query per run, which is N
//! queries for a page of runs, and a trait that can only be asked about one task
//! at a time would have made that shape the only one available. Asking for many
//! makes the batched form the default and the per-run form
//! ([`cost_for_run`]) the special case.
//!
//! # Why a total over partly-unknown runs is a floor
//!
//! [`SpendTotal::known_usd`] is a sum over the runs whose cost is known. If any
//! run's cost is unknown, that sum is a *lower bound* on what was actually
//! spent, not a total — which is why [`SpendTotal::unknown_runs`] travels beside
//! it and [`SpendTotal::is_floor`] exists. The page renders a floor as `≥$1.20`.
//! A caller that reads `known_usd` alone and prints it as an exact figure has
//! reintroduced the very defect this module prevents, one level up.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::warn;

use magicllm::LlmScope;

use magician::magician_v2::analytics::llm_analytics_read_service::LlmAnalyticsReadService;

use super::run::EvalRun;

/// Task ids per ledger query.
///
/// The grouped query returns at most two rows per task (a priced group and an
/// unpriced one), and the read service truncates hard at 1,000 rows, so this has
/// to stay comfortably below 500 or a full page of runs would come back
/// silently short — and a task whose rows were truncated away is
/// indistinguishable from a task with no rows, which is exactly the
/// unknown-renders-as-zero defect. 400 leaves 800 rows against a 1,000 ceiling.
const MAX_TASK_IDS_PER_LEDGER_QUERY: usize = 400;

/// The widest window the LLM ledger will answer for, mirroring its own bound.
///
/// Checked here so an over-wide range is refused with a legible reason instead
/// of surfacing as a generic query failure — the outcome is the same (unknown,
/// never zero), but the caller can tell the difference between "the ledger is
/// down" and "you asked for six months".
pub const LEDGER_MAX_WINDOW_MS: i64 = 31 * 24 * 60 * 60 * 1_000;

/// What a run cost, or the honest admission that we do not know.
///
/// There is deliberately no `Known(0.0)`-shaped default: `Unknown` is not a
/// number and cannot be summed, averaged, or formatted as currency by accident.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(into = "CostValueWire", from = "CostValueWire")]
pub enum CostValue {
    /// The ledger answered. `Known(0.0)` is a real answer: the run made no
    /// billable calls. It is NOT the same fact as [`CostValue::Unknown`].
    Known(f64),
    /// The ledger could not be asked, could not answer, or the run has no task
    /// to attribute spend to. Renders as `—`, never as `$0.00`.
    Unknown,
}

impl CostValue {
    /// The figure, when there is one. `None` for [`CostValue::Unknown`] — which
    /// is the point: a caller that wants a number has to say what it will do
    /// when there isn't one.
    pub const fn usd(self) -> Option<f64> {
        match self {
            Self::Known(usd) => Some(usd),
            Self::Unknown => None,
        }
    }

    pub const fn is_known(self) -> bool {
        matches!(self, Self::Known(_))
    }
}

/// The JSON shape: `{"kind":"known","usd":0.83}` / `{"kind":"unknown"}`.
///
/// A tagged object rather than a nullable number, because `null` and `0` are one
/// typo apart in a template and the whole point is that they are different
/// facts. The UI branches on `kind`; there is no numeric field to reach for when
/// the answer is unknown.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CostValueWire {
    Known { usd: f64 },
    Unknown,
}

impl From<CostValue> for CostValueWire {
    fn from(value: CostValue) -> Self {
        match value {
            CostValue::Known(usd) => Self::Known { usd },
            CostValue::Unknown => Self::Unknown,
        }
    }
}

impl From<CostValueWire> for CostValue {
    fn from(value: CostValueWire) -> Self {
        match value {
            CostValueWire::Known { usd } => Self::Known(usd),
            CostValueWire::Unknown => Self::Unknown,
        }
    }
}

/// One contribution to a task's spend, as the ledger reports it.
///
/// An implementation may roll many calls into one row. What it must never roll
/// away is a `cost_usd` of `None`: that is a call the ledger has but cannot
/// price, and summing it as zero would understate real money. It stays a
/// separate row precisely so the fold can see it.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerCostRow {
    pub task_id: String,
    /// The row's cost. `None` means the ledger holds the call but not its price
    /// — unknown, not free.
    pub cost_usd: Option<f64>,
}

impl LedgerCostRow {
    pub fn new(task_id: impl Into<String>, cost_usd: Option<f64>) -> Self {
        Self {
            task_id: task_id.into(),
            cost_usd,
        }
    }
}

/// The ledger could not answer. Always becomes [`CostValue::Unknown`], never a
/// zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerUnavailable {
    pub reason: String,
}

impl LedgerUnavailable {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for LedgerUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.reason)
    }
}

impl std::error::Error for LedgerUnavailable {}

/// The money side of the join, narrowed to the one question this module asks.
///
/// Deliberately plural in `task_ids`: see the module docs. `from_ms`/`to_ms` is
/// a half-open window that the caller has already sized to cover the runs being
/// priced, so an implementation may use it to prune partitions but must not use
/// it to filter beyond what was asked.
pub trait EvalCostLedger {
    /// Every priced contribution the ledger holds for `task_ids` in the window.
    ///
    /// Returning no rows for a task must mean "this task made no billable
    /// calls" — an implementation that cannot distinguish that from "I did not
    /// look" MUST return `Err` instead, because the caller will read absence as
    /// a genuine zero.
    fn cost_rows_for_tasks(
        &self,
        task_ids: &[&str],
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<LedgerCostRow>, LedgerUnavailable>;
}

/// One run's cost, carrying enough identity to be zipped back onto a page of
/// runs without relying on ordering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCost {
    pub run_id: String,
    pub lane_id: String,
    pub cost: CostValue,
}

/// Spend over a set of runs, and how much of it we are sure about.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SpendTotal {
    /// Sum of the runs whose cost IS known. Money spent by one task is counted
    /// once however many runs name that task.
    pub known_usd: f64,
    /// How many runs contributed an unknown cost. Non-zero ⇒ `known_usd` is a
    /// FLOOR, not a total, and must never be rendered as if it were exact.
    pub unknown_runs: usize,
    pub total_runs: usize,
}

impl SpendTotal {
    /// Whether [`SpendTotal::known_usd`] is a lower bound rather than a total.
    /// Zero runs is not a floor: no runs genuinely is no spend.
    pub const fn is_floor(self) -> bool {
        self.unknown_runs > 0
    }
}

/// One lane's slice of a [`SpendReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneSpend {
    pub lane_id: String,
    pub total: SpendTotal,
}

/// Server-side answer to "what did the evals spend over this range" — the whole
/// point being that the page does not have to fetch a thousand runs and fold
/// them client-side, where it can only ever total the ones it happened to fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendReport {
    pub from_ms: i64,
    pub to_ms: i64,
    pub total: SpendTotal,
    /// Sorted by lane id, so the rendered order is stable between reloads.
    ///
    /// When one task ran several lanes, `total.known_usd` counts its money once
    /// while each lane counts it within itself, so the lane figures can sum to
    /// more than the whole-range total. The whole-range total is the one that is
    /// right about how much was spent.
    pub by_lane: Vec<LaneSpend>,
}

/// What one run cost. Convenience over [`costs_for_runs`]; prefer the batched
/// form for anything rendering more than a single row.
pub fn cost_for_run(run: &EvalRun, ledger: &dyn EvalCostLedger) -> CostValue {
    join(&[run], ledger)
        .into_iter()
        .next()
        .unwrap_or(CostValue::Unknown)
}

/// What each run cost, in the order given, in ONE ledger query.
///
/// Note the absence of a `Result`: a ledger failure is data (every run becomes
/// [`CostValue::Unknown`]), not an error the caller could accidentally
/// `unwrap_or_default()` into a page of free lanes.
pub fn costs_for_runs(runs: &[EvalRun], ledger: &dyn EvalCostLedger) -> Vec<RunCost> {
    let borrowed = runs.iter().collect::<Vec<_>>();
    join(&borrowed, ledger)
        .into_iter()
        .zip(runs)
        .map(|(cost, run)| RunCost {
            run_id: run.run_id.clone(),
            lane_id: run.lane_id.clone(),
            cost,
        })
        .collect()
}

/// Spend over the runs that STARTED in `[from_ms, to_ms)`, whole-range and
/// per-lane.
///
/// Filtering on start (not on overlap) is what makes each run belong to exactly
/// one range, so consecutive ranges partition history instead of double-counting
/// the runs that straddle a boundary.
pub fn spend_in_range(
    runs: &[EvalRun],
    ledger: &dyn EvalCostLedger,
    from_ms: i64,
    to_ms: i64,
) -> SpendReport {
    let in_range = runs
        .iter()
        .filter(|run| run.started_at_ms >= from_ms && run.started_at_ms < to_ms)
        .collect::<Vec<_>>();
    let costs = join(&in_range, ledger);

    let mut lanes: Vec<String> = in_range.iter().map(|run| run.lane_id.clone()).collect();
    lanes.sort_unstable();
    lanes.dedup();

    let paired = || in_range.iter().copied().zip(costs.iter().copied());
    let by_lane: Vec<LaneSpend> = lanes
        .into_iter()
        .map(|lane_id| {
            let total = total_over(paired().filter(|(run, _)| run.lane_id == lane_id));
            LaneSpend { lane_id, total }
        })
        .collect();

    SpendReport {
        from_ms,
        to_ms,
        total: total_over(paired()),
        by_lane,
    }
}

/// The shared join. One ledger call for the whole set, one answer per run, in
/// order.
fn join(runs: &[&EvalRun], ledger: &dyn EvalCostLedger) -> Vec<CostValue> {
    let all_unknown = || vec![CostValue::Unknown; runs.len()];

    let mut task_ids = runs
        .iter()
        .filter_map(|run| run.task_id.as_deref())
        .collect::<Vec<_>>();
    task_ids.sort_unstable();
    task_ids.dedup();
    if task_ids.is_empty() {
        // Nothing to attribute spend to. Asking the ledger about no tasks would
        // answer "no rows", which folds to a zero — so we must not ask.
        return all_unknown();
    }

    let Some((from_ms, to_ms)) = ledger_window(runs) else {
        return all_unknown();
    };
    let rows = match ledger.cost_rows_for_tasks(&task_ids, from_ms, to_ms) {
        Ok(rows) => rows,
        Err(error) => {
            // Loud, because the page will render `—` for lanes that really did
            // spend money and the reason must be findable.
            warn!(
                %error,
                tasks = task_ids.len(),
                from_ms,
                to_ms,
                "LLM ledger unavailable; eval run cost is unknown (NOT zero)"
            );
            return all_unknown();
        },
    };

    let by_task = fold_rows(&task_ids, &rows);
    runs.iter()
        .map(|run| {
            // Fixture calls happen in a separate evaluator process. The task
            // ledger covers its launcher, not those calls; report estimates
            // must not masquerade as a reconciled total here.
            if run.lane_id == super::options::LIFECYCLE_LIVE {
                return CostValue::Unknown;
            }
            run.task_id
                .as_deref()
                .and_then(|task_id| by_task.get(task_id).copied())
                .unwrap_or(CostValue::Unknown)
        })
        .collect()
}

/// Ledger rows to one cost per task asked about.
///
/// Every asked-about task starts at `Known(0.0)` — absence of rows IS the answer
/// "no billable calls", and it is only trustworthy because the caller has
/// already established that the ledger answered at all.
fn fold_rows(task_ids: &[&str], rows: &[LedgerCostRow]) -> HashMap<String, CostValue> {
    let mut costs = task_ids
        .iter()
        .map(|task_id| ((*task_id).to_string(), CostValue::Known(0.0)))
        .collect::<HashMap<_, _>>();

    for row in rows {
        // A row for something we did not ask about is somebody else's money.
        let Some(cost) = costs.get_mut(&row.task_id) else {
            continue;
        };
        let CostValue::Known(running) = *cost else {
            // Once unknown, always unknown: a later priced row cannot restore
            // confidence in a total that is already missing a piece.
            continue;
        };
        *cost = match row.cost_usd {
            // The ledger has the call but not its price. Adding nothing would
            // silently understate the run; this is the null-is-not-zero case.
            None => CostValue::Unknown,
            // Written non-negative and finite by the recorder, so anything else
            // is corruption or drift. Refusing to add it is the fail-closed
            // reading: a figure we cannot trust is not a figure.
            Some(usd) if !usd.is_finite() || usd < 0.0 => CostValue::Unknown,
            Some(usd) => CostValue::Known(running + usd),
        };
    }
    costs
}

/// The window that covers every LLM call the runs could have made.
///
/// Derived from the runs rather than from the caller's requested range because a
/// run that started one millisecond before the range ended kept spending after
/// it: `started_at_ms` alone would cut its calls off, and cut-off calls do not
/// look like an error — they look like a smaller bill.
///
/// `None` when there is nothing to ask about.
fn ledger_window(runs: &[&EvalRun]) -> Option<(i64, i64)> {
    let attributable = || runs.iter().filter(|run| run.task_id.is_some());
    let from_ms = attributable().map(|run| run.started_at_ms).min()?.max(0);
    let to_ms = attributable()
        .map(|run| {
            run.started_at_ms
                .saturating_add(run.duration_ms.max(0))
                .max(from_ms)
        })
        .max()?
        .saturating_add(1);
    Some((from_ms, to_ms))
}

/// Folds costs into a total, counting each task's money once.
///
/// Two runs naming the same task spent that money between them, not twice. The
/// run COUNTS stay per-run (`total_runs`/`unknown_runs` describe rows on the
/// page); only the dollars are deduplicated.
fn total_over<'a>(entries: impl Iterator<Item = (&'a EvalRun, CostValue)>) -> SpendTotal {
    let mut seen_tasks: HashSet<&'a str> = HashSet::new();
    let mut total = SpendTotal::default();
    for (run, cost) in entries {
        total.total_runs += 1;
        match cost {
            CostValue::Unknown => total.unknown_runs += 1,
            CostValue::Known(usd) => {
                // A run with no task id is always Unknown, so `true` here is the
                // unreachable arm rather than a silent pass-through.
                let first_sighting = run
                    .task_id
                    .as_deref()
                    .is_none_or(|task_id| seen_tasks.insert(task_id));
                if first_sighting {
                    total.known_usd += usd;
                }
            },
        }
    }
    total
}

/// The real ledger: the governed LLM analytics store, scoped.
///
/// Every operating decision in here exists to protect the same invariant — when
/// this adapter is not certain it saw everything, it returns `Err` (which the
/// join turns into unknown) rather than a short answer (which the join would
/// turn into a smaller bill).
pub struct LlmLedgerCosts {
    service: Arc<LlmAnalyticsReadService>,
    scope: LlmScope,
}

impl LlmLedgerCosts {
    pub fn new(
        service: Arc<LlmAnalyticsReadService>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            service,
            scope: LlmScope::new(principal, workspace),
        }
    }

    fn query_chunk(
        &self,
        task_ids: &[&str],
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<LedgerCostRow>, LedgerUnavailable> {
        let mut literals = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            if !is_safe_sql_identifier_literal(task_id) {
                // Fail the batch rather than dropping the id: a dropped id comes
                // back as "no rows", which the fold reads as a genuine zero.
                return Err(LedgerUnavailable::new(format!(
                    "task id `{task_id}` is not a shape this join can safely ask about"
                )));
            }
            literals.push(format!("'{task_id}'"));
        }

        // GROUPing by `cost_usd IS NULL` as well as by task splits each task into
        // at most two rows: the priced calls, whose `sum` is their real total,
        // and the unpriced ones, whose `sum` over all-NULLs is itself NULL. That
        // NULL is the whole reason for the second group — it is what stops an
        // unpriced call from being summed away into a confident, wrong figure.
        let sql = format!(
            "SELECT task_id, sum(cost_usd) AS cost_usd FROM llm_calls \
             WHERE task_id IN ({}) GROUP BY task_id, (cost_usd IS NULL)",
            literals.join(", ")
        );
        // At most two rows per task; one more so a full result is distinguishable
        // from a truncated one.
        let limit = task_ids.len().saturating_mul(2).saturating_add(1);

        let envelope = self
            .service
            .query_fact_sql(&self.scope, &sql, Some(from_ms), Some(to_ms), Some(limit))
            .map_err(|error| {
                LedgerUnavailable::new(format!("LLM ledger query failed: {error:#}"))
            })?;
        let page = envelope.data;
        if page.row_count >= page.limit {
            // Cannot happen with the limit above, and would be catastrophic if it
            // did: the tasks whose rows fell off the end would price as free.
            return Err(LedgerUnavailable::new(format!(
                "LLM ledger returned {} rows against a {} row limit; \
                 the result may be truncated and a truncated cost is not a cost",
                page.row_count, page.limit
            )));
        }

        page.rows
            .into_iter()
            .map(|row| {
                let task_id = row
                    .get("task_id")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| {
                        LedgerUnavailable::new("LLM ledger row has no readable task_id")
                    })?
                    .to_string();
                let raw_cost = row.get("cost_usd").ok_or_else(|| {
                    LedgerUnavailable::new("LLM ledger row has no cost_usd column")
                })?;
                // A missing column is schema drift (fail); a NULL value is a real
                // answer meaning "priced? unknown". They must not be conflated.
                let cost_usd = if raw_cost.is_null() {
                    None
                } else {
                    Some(raw_cost.as_f64().ok_or_else(|| {
                        LedgerUnavailable::new(format!(
                            "LLM ledger cost_usd is not a number: {raw_cost}"
                        ))
                    })?)
                };
                Ok(LedgerCostRow { task_id, cost_usd })
            })
            .collect()
    }
}

impl EvalCostLedger for LlmLedgerCosts {
    fn cost_rows_for_tasks(
        &self,
        task_ids: &[&str],
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<LedgerCostRow>, LedgerUnavailable> {
        if from_ms < 0 || to_ms <= from_ms {
            return Err(LedgerUnavailable::new(format!(
                "eval cost needs a positive half-open window, got [{from_ms}, {to_ms})"
            )));
        }
        if to_ms.saturating_sub(from_ms) > LEDGER_MAX_WINDOW_MS {
            // Refused rather than silently answered for the part of the range the
            // ledger would accept: a partial answer is a smaller bill wearing the
            // clothes of a total.
            return Err(LedgerUnavailable::new(format!(
                "the LLM ledger answers at most {} days at a time; \
                 narrow the range rather than reading a partial total",
                LEDGER_MAX_WINDOW_MS / (24 * 60 * 60 * 1_000)
            )));
        }

        let mut rows = Vec::new();
        for chunk in task_ids.chunks(MAX_TASK_IDS_PER_LEDGER_QUERY) {
            // A failed chunk fails the whole batch. Keeping the chunks that
            // worked would leave the rest looking like tasks with no rows, which
            // the fold reads as genuinely free.
            rows.extend(self.query_chunk(chunk, from_ms, to_ms)?);
        }
        Ok(rows)
    }
}

/// Whether a task id can be inlined into a SQL string literal without escaping.
///
/// An allowlist rather than quote-doubling: the read service's SQL surface is
/// AST-validated but this is still caller data reaching a query builder, and the
/// set of ids the task system actually mints (ULIDs, `task_*`) is comfortably
/// inside this. Anything else is refused loudly, never quietly dropped.
fn is_safe_sql_identifier_literal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    /// A ledger whose answers the test dictates, and which records what it was
    /// asked — the batching claim in the module docs is only true if something
    /// checks it.
    struct FakeLedger {
        answer: Result<Vec<LedgerCostRow>, LedgerUnavailable>,
        asked: RefCell<Vec<(Vec<String>, i64, i64)>>,
    }

    impl FakeLedger {
        fn with_rows(rows: Vec<LedgerCostRow>) -> Self {
            Self {
                answer: Ok(rows),
                asked: RefCell::new(Vec::new()),
            }
        }

        fn empty() -> Self {
            Self::with_rows(Vec::new())
        }

        fn failing() -> Self {
            Self {
                answer: Err(LedgerUnavailable::new("timed out waiting for the guard")),
                asked: RefCell::new(Vec::new()),
            }
        }

        fn query_count(&self) -> usize {
            self.asked.borrow().len()
        }
    }

    impl EvalCostLedger for FakeLedger {
        fn cost_rows_for_tasks(
            &self,
            task_ids: &[&str],
            from_ms: i64,
            to_ms: i64,
        ) -> Result<Vec<LedgerCostRow>, LedgerUnavailable> {
            self.asked.borrow_mut().push((
                task_ids.iter().map(|id| (*id).to_string()).collect(),
                from_ms,
                to_ms,
            ));
            self.answer.clone()
        }
    }

    #[test]
    fn lifecycle_external_calls_are_unknown_even_when_launcher_cost_is_known() {
        let run = run_in(
            super::super::options::LIFECYCLE_LIVE,
            "eval-1",
            Some("task-1"),
            1_000,
        );
        for ledger in [
            FakeLedger::empty(),
            FakeLedger::with_rows(vec![LedgerCostRow::new("task-1", Some(0.01))]),
        ] {
            assert_eq!(cost_for_run(&run, &ledger), CostValue::Unknown);
            let total = spend_in_range(&[run.clone()], &ledger, 0, 100_000).total;
            assert_eq!(total.unknown_runs, 1);
        }
    }

    fn run_with_task(task_id: &str) -> EvalRun {
        let mut run = EvalRun::new("evr_1", "lane-a", "anonymous", "default", 1_000);
        run.task_id = Some(task_id.to_string());
        run.duration_ms = 60_000;
        run
    }

    fn run_in(lane: &str, run_id: &str, task_id: Option<&str>, started_at_ms: i64) -> EvalRun {
        let mut run = EvalRun::new(run_id, lane, "anonymous", "default", started_at_ms);
        run.task_id = task_id.map(str::to_string);
        run.duration_ms = 1_000;
        run
    }

    /// f64 sums are not exact; the fact under test is the arithmetic, not the
    /// last bit of the mantissa.
    fn assert_known(cost: CostValue, expected_usd: f64) {
        match cost {
            CostValue::Known(usd) => assert!(
                (usd - expected_usd).abs() < 1e-9,
                "expected ${expected_usd}, got ${usd}"
            ),
            CostValue::Unknown => panic!("expected a known ${expected_usd}, got Unknown"),
        }
    }

    #[test]
    fn cost_sums_ledger_rows_for_the_runs_task_id() {
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_42", Some(1.25)),
            LedgerCostRow::new("task_42", Some(0.75)),
        ]);
        assert_known(cost_for_run(&run_with_task("task_42"), &ledger), 2.0);
    }

    #[test]
    fn a_run_with_no_task_id_has_unknown_cost() {
        // Spend cannot be attributed, so there is no honest number to show.
        let run = EvalRun::new("evr_1", "lane-a", "anonymous", "default", 1_000);
        assert_eq!(run.task_id, None);
        let ledger = FakeLedger::with_rows(vec![LedgerCostRow::new("task_42", Some(9.99))]);
        assert_eq!(cost_for_run(&run, &ledger), CostValue::Unknown);
        assert_eq!(
            ledger.query_count(),
            0,
            "with nothing to attribute, asking would answer `no rows` — i.e. zero"
        );
    }

    /// THE most important behaviour in this feature: a ledger failure renders as
    /// unknown, never as free. A lane that spent $2 must never display $0.00
    /// because a query failed.
    #[test]
    fn a_ledger_failure_yields_unknown_not_zero() {
        let cost = cost_for_run(&run_with_task("task_42"), &FakeLedger::failing());
        assert_eq!(cost, CostValue::Unknown);
        assert_ne!(cost, CostValue::Known(0.0));
        assert_eq!(cost.usd(), None, "there must be no number to format");
    }

    /// "made no calls" and "could not ask" are different answers and must not collapse.
    #[test]
    fn a_run_that_genuinely_made_no_calls_is_known_zero() {
        let run = run_with_task("task_42");
        let genuinely_free = cost_for_run(&run, &FakeLedger::empty());
        assert_eq!(genuinely_free, CostValue::Known(0.0));

        let could_not_ask = cost_for_run(&run, &FakeLedger::failing());
        assert_ne!(
            genuinely_free, could_not_ask,
            "a free run and an unmeasurable one must never be the same value"
        );
    }

    #[test]
    fn rows_for_other_task_ids_are_not_counted() {
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_42", Some(1.0)),
            LedgerCostRow::new("task_other", Some(99.0)),
            LedgerCostRow::new("", Some(5.0)),
        ]);
        assert_known(cost_for_run(&run_with_task("task_42"), &ledger), 1.0);
    }

    #[test]
    fn a_row_with_a_null_cost_does_not_silently_become_zero() {
        // The ledger has the call but not its price: the run's total is missing a
        // piece, so the total is not known — not "known to be the rest".
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_42", Some(2.0)),
            LedgerCostRow::new("task_42", None),
        ]);
        assert_eq!(
            cost_for_run(&run_with_task("task_42"), &ledger),
            CostValue::Unknown
        );

        // And not merely by luck of ordering.
        let reversed = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_42", None),
            LedgerCostRow::new("task_42", Some(2.0)),
        ]);
        assert_eq!(
            cost_for_run(&run_with_task("task_42"), &reversed),
            CostValue::Unknown
        );
    }

    /// The recorder writes cost finite and non-negative, so anything else is
    /// corruption. A figure we cannot trust is not a figure.
    #[test]
    fn a_nonsensical_cost_is_unknown_rather_than_counted() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            let ledger = FakeLedger::with_rows(vec![
                LedgerCostRow::new("task_42", Some(1.0)),
                LedgerCostRow::new("task_42", Some(bad)),
            ]);
            assert_eq!(
                cost_for_run(&run_with_task("task_42"), &ledger),
                CostValue::Unknown,
                "cost {bad} must not be folded into a total"
            );
        }
    }

    /// The UI branches on `kind`. A nullable number would put `null` and `0` one
    /// typo apart, which is the entire defect this module exists to prevent.
    #[test]
    fn cost_serializes_as_a_tagged_kind_with_no_number_when_unknown() {
        let known = serde_json::to_value(CostValue::Known(0.834)).unwrap();
        assert_eq!(known["kind"], "known");
        assert_eq!(known["usd"], 0.834);

        let zero = serde_json::to_value(CostValue::Known(0.0)).unwrap();
        assert_eq!(zero["kind"], "known");
        assert_eq!(zero["usd"], 0.0);

        let unknown = serde_json::to_value(CostValue::Unknown).unwrap();
        assert_eq!(unknown["kind"], "unknown");
        assert!(
            unknown.get("usd").is_none(),
            "an unknown cost must offer no number to render"
        );
        assert_ne!(unknown, zero);

        for value in [
            CostValue::Known(1.5),
            CostValue::Known(0.0),
            CostValue::Unknown,
        ] {
            let round_tripped: CostValue =
                serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap();
            assert_eq!(round_tripped, value);
        }
    }

    /// The batching claim in the module docs, enforced: a page of runs is one
    /// query, not one per run.
    #[test]
    fn a_page_of_runs_costs_one_ledger_query() {
        let runs = vec![
            run_in("lane-a", "evr_1", Some("task_1"), 10),
            run_in("lane-a", "evr_2", Some("task_2"), 20),
            run_in("lane-b", "evr_3", Some("task_3"), 30),
            // A repeat of an id already asked about must not add a question.
            run_in("lane-b", "evr_4", Some("task_1"), 40),
        ];
        let ledger = FakeLedger::with_rows(vec![LedgerCostRow::new("task_1", Some(1.0))]);
        let _ = costs_for_runs(&runs, &ledger);

        assert_eq!(ledger.query_count(), 1);
        let (asked, _, _) = ledger.asked.borrow()[0].clone();
        assert_eq!(asked, vec!["task_1", "task_2", "task_3"]);
    }

    #[test]
    fn every_run_gets_an_answer_in_the_order_it_was_given() {
        let runs = vec![
            run_in("lane-a", "evr_1", Some("task_1"), 10),
            run_in("lane-b", "evr_2", None, 20),
            run_in("lane-a", "evr_3", Some("task_2"), 30),
        ];
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_1", Some(1.5)),
            LedgerCostRow::new("task_2", Some(0.5)),
        ]);
        let costs = costs_for_runs(&runs, &ledger);

        assert_eq!(
            costs.iter().map(|c| c.run_id.as_str()).collect::<Vec<_>>(),
            vec!["evr_1", "evr_2", "evr_3"]
        );
        assert_eq!(costs[0].lane_id, "lane-a");
        assert_known(costs[0].cost, 1.5);
        assert_eq!(costs[1].cost, CostValue::Unknown);
        assert_known(costs[2].cost, 0.5);
    }

    #[test]
    fn a_ledger_failure_makes_every_run_on_the_page_unknown_not_free() {
        let runs = vec![
            run_in("lane-a", "evr_1", Some("task_1"), 10),
            run_in("lane-b", "evr_2", Some("task_2"), 20),
        ];
        let costs = costs_for_runs(&runs, &FakeLedger::failing());
        assert!(costs.iter().all(|c| c.cost == CostValue::Unknown));
        assert!(costs.iter().all(|c| c.cost.usd().is_none()));
    }

    /// A run keeps spending after it starts, so a window cut at `started_at_ms`
    /// would drop the calls it made while running — and dropped calls do not look
    /// like an error, they look like a smaller bill.
    #[test]
    fn the_ledger_window_spans_the_whole_life_of_every_run() {
        let mut early = run_in("lane-a", "evr_1", Some("task_1"), 1_000);
        early.duration_ms = 500;
        let mut late = run_in("lane-a", "evr_2", Some("task_2"), 5_000);
        late.duration_ms = 60_000;

        let ledger = FakeLedger::empty();
        let _ = costs_for_runs(&[early, late], &ledger);

        let (_, from_ms, to_ms) = ledger.asked.borrow()[0].clone();
        assert_eq!(from_ms, 1_000, "must start at the earliest run");
        assert_eq!(
            to_ms, 65_001,
            "must extend past the END of the longest-running run, exclusively"
        );
    }

    #[test]
    fn spend_over_runs_whose_cost_is_all_known_is_an_exact_total() {
        let runs = vec![
            run_in("lane-a", "evr_1", Some("task_1"), 10),
            run_in("lane-a", "evr_2", Some("task_2"), 20),
        ];
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_1", Some(1.0)),
            LedgerCostRow::new("task_2", Some(0.2)),
        ]);
        let report = spend_in_range(&runs, &ledger, 0, 100);

        assert!((report.total.known_usd - 1.2).abs() < 1e-9);
        assert_eq!(report.total.unknown_runs, 0);
        assert_eq!(report.total.total_runs, 2);
        assert!(
            !report.total.is_floor(),
            "nothing was unknown, so this is a total and may be rendered exactly"
        );
    }

    /// A total over runs where SOME cost is unknown is a FLOOR. The caller must
    /// be unable to mistake it for an exact figure.
    #[test]
    fn spend_with_any_unknown_run_is_a_floor_not_a_total() {
        let runs = vec![
            run_in("lane-a", "evr_1", Some("task_1"), 10),
            run_in("lane-a", "evr_2", Some("task_2"), 20),
            run_in("lane-b", "evr_3", None, 30),
        ];
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_1", Some(1.0)),
            LedgerCostRow::new("task_2", Some(0.2)),
        ]);
        let report = spend_in_range(&runs, &ledger, 0, 100);

        assert!((report.total.known_usd - 1.2).abs() < 1e-9);
        assert_eq!(report.total.unknown_runs, 1);
        assert_eq!(report.total.total_runs, 3);
        assert!(
            report.total.is_floor(),
            "1.2 is a lower bound here and must not render as `$1.20`"
        );
    }

    #[test]
    fn spend_over_runs_whose_cost_is_all_unknown_is_zero_known_and_a_floor() {
        let runs = vec![
            run_in("lane-a", "evr_1", Some("task_1"), 10),
            run_in("lane-b", "evr_2", Some("task_2"), 20),
        ];
        let report = spend_in_range(&runs, &FakeLedger::failing(), 0, 100);

        assert_eq!(report.total.known_usd, 0.0);
        assert_eq!(report.total.unknown_runs, 2);
        assert_eq!(report.total.total_runs, 2);
        assert!(
            report.total.is_floor(),
            "$0.00 over two unmeasured runs is a floor, not a bill of nothing"
        );
    }

    /// No runs genuinely IS no spend — which is why the empty case is
    /// `unknown_runs: 0` and not a floor.
    #[test]
    fn an_empty_range_is_no_spend_rather_than_unknown_spend() {
        let runs = vec![run_in("lane-a", "evr_1", Some("task_1"), 10)];
        let ledger = FakeLedger::with_rows(vec![LedgerCostRow::new("task_1", Some(5.0))]);
        let report = spend_in_range(&runs, &ledger, 1_000, 2_000);

        assert_eq!(report.total.known_usd, 0.0);
        assert_eq!(report.total.unknown_runs, 0);
        assert_eq!(report.total.total_runs, 0);
        assert!(!report.total.is_floor());
        assert!(report.by_lane.is_empty());
        assert_eq!((report.from_ms, report.to_ms), (1_000, 2_000));
        assert_eq!(
            ledger.query_count(),
            0,
            "no runs in range means nothing to ask about"
        );
    }

    #[test]
    fn spend_is_broken_down_per_lane_in_a_stable_order() {
        let runs = vec![
            run_in("lane-b", "evr_1", Some("task_1"), 10),
            run_in("lane-a", "evr_2", Some("task_2"), 20),
            run_in("lane-a", "evr_3", Some("task_3"), 30),
        ];
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_1", Some(4.0)),
            LedgerCostRow::new("task_2", Some(1.0)),
            // task_3 has no rows: a genuine zero, and it must stay a zero.
        ]);
        let report = spend_in_range(&runs, &ledger, 0, 100);

        assert_eq!(
            report
                .by_lane
                .iter()
                .map(|lane| lane.lane_id.as_str())
                .collect::<Vec<_>>(),
            vec!["lane-a", "lane-b"],
            "sorted, so the rendered order does not shuffle between reloads"
        );
        assert!((report.by_lane[0].total.known_usd - 1.0).abs() < 1e-9);
        assert_eq!(report.by_lane[0].total.total_runs, 2);
        assert!(!report.by_lane[0].total.is_floor());
        assert!((report.by_lane[1].total.known_usd - 4.0).abs() < 1e-9);
        assert!((report.total.known_usd - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_range_covers_the_runs_that_started_inside_it_and_no_others() {
        let runs = vec![
            run_in("lane-a", "evr_before", Some("task_1"), 99),
            run_in("lane-a", "evr_inside", Some("task_2"), 100),
            run_in("lane-a", "evr_after", Some("task_3"), 200),
        ];
        let ledger = FakeLedger::with_rows(vec![
            LedgerCostRow::new("task_1", Some(1.0)),
            LedgerCostRow::new("task_2", Some(2.0)),
            LedgerCostRow::new("task_3", Some(4.0)),
        ]);
        // Half-open: 100 is in, 200 is out, so consecutive ranges partition
        // history rather than double-counting the boundary.
        let report = spend_in_range(&runs, &ledger, 100, 200);

        assert_eq!(report.total.total_runs, 1);
        assert!((report.total.known_usd - 2.0).abs() < 1e-9);
        let (asked, _, _) = ledger.asked.borrow()[0].clone();
        assert_eq!(
            asked,
            vec!["task_2"],
            "out-of-range tasks are not even asked"
        );
    }

    /// Money spent by one task is spent once, however many run records name it.
    /// Counting it per-run would inflate the bill in the expensive direction.
    #[test]
    fn two_runs_sharing_a_task_do_not_double_count_its_money() {
        let runs = vec![
            run_in("lane-a", "evr_1", Some("task_shared"), 10),
            run_in("lane-a", "evr_2", Some("task_shared"), 20),
        ];
        let ledger = FakeLedger::with_rows(vec![LedgerCostRow::new("task_shared", Some(3.0))]);
        let report = spend_in_range(&runs, &ledger, 0, 100);

        assert!((report.total.known_usd - 3.0).abs() < 1e-9);
        assert_eq!(report.total.total_runs, 2, "both runs are still rows");
        assert_eq!(report.total.unknown_runs, 0);

        // Each run still reports what its task cost; only the total deduplicates.
        let per_run = costs_for_runs(&runs, &ledger);
        assert_known(per_run[0].cost, 3.0);
        assert_known(per_run[1].cost, 3.0);
    }

    #[test]
    fn a_task_id_that_could_not_be_asked_about_safely_is_refused_not_dropped() {
        // Dropping it would return "no rows", which the fold reads as a genuine
        // zero — the exact substitution this module forbids.
        assert!(is_safe_sql_identifier_literal("task_42"));
        assert!(is_safe_sql_identifier_literal("01JGXQ7YQ4Z8V1N2K3M4P5R6S7"));
        assert!(is_safe_sql_identifier_literal("01JGX:a1"));
        assert!(!is_safe_sql_identifier_literal(""));
        assert!(!is_safe_sql_identifier_literal("task' OR '1'='1"));
        assert!(!is_safe_sql_identifier_literal("task\"42"));
        assert!(!is_safe_sql_identifier_literal(&"x".repeat(129)));
    }

    /// The real adapter's fail-closed guards, reachable without a database
    /// because every one of them fires BEFORE a query is issued.
    #[test]
    fn the_adapter_refuses_what_it_cannot_answer_in_full() {
        let dir = tempfile::tempdir().unwrap();
        let service = Arc::new(LlmAnalyticsReadService::new(ArtifactV2Workspace::new(
            dir.path(),
        )));
        let ledger = LlmLedgerCosts::new(service, "anonymous", "default");

        assert!(
            ledger
                .cost_rows_for_tasks(&["task_1"], 0, LEDGER_MAX_WINDOW_MS + 2)
                .is_err(),
            "a range wider than the ledger answers must not come back partially summed"
        );
        assert!(ledger.cost_rows_for_tasks(&["task_1"], 500, 500).is_err());
        assert!(ledger.cost_rows_for_tasks(&["task_1"], -1, 500).is_err());
        assert!(
            ledger
                .cost_rows_for_tasks(&["task'; --"], 0, 1_000)
                .is_err(),
            "an unaskable id must fail the batch, not quietly leave it"
        );

        // And each of those failures reaches the join as unknown, never as free.
        let run = run_with_task("task_1");
        assert_eq!(cost_for_run(&run, &FailingWindow), CostValue::Unknown);
    }

    struct FailingWindow;

    impl EvalCostLedger for FailingWindow {
        fn cost_rows_for_tasks(
            &self,
            _task_ids: &[&str],
            _from_ms: i64,
            _to_ms: i64,
        ) -> Result<Vec<LedgerCostRow>, LedgerUnavailable> {
            Err(LedgerUnavailable::new("window refused"))
        }
    }
}
