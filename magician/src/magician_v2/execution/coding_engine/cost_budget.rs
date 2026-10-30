//! Hard spending cap for one VibeDev run's coding turns.
//!
//! The owner sets a dollar budget per run (`- Cost budget: $X` in the task's
//! control region). Until this existed the only thing honouring it was the
//! coordinator reading `session_stats.cost` and deciding to stop — a budget
//! that holds only while the model obeys prose.
//!
//! Two checks now hold it in code:
//!
//! - **Between turns, every engine.** `run_coding_task` refuses to start a turn
//!   once the run's recorded spend has reached the cap.
//! - **Inside a turn, where the engine reports cost as it goes.** Pi stamps
//!   each finished assistant message with its USD cost, and the turn's
//!   [`ProgressWatchdog`](super::budgets::ProgressWatchdog) stops the turn on the
//!   first message that takes the run to the cap. The other engines report
//!   cost once, when the turn ends, so for them the cap bites at the next turn.
//!
//! Spend is what the engines report. A turn whose engine reports no dollar cost
//! (a subscription Codex turn, most Grok turns) is counted as *unpriced* and
//! surfaced in telemetry rather than guessed at.
//!
//! Turns of the same run that run in parallel share one in-process meter, so
//! each sees the others' live spend rather than only what had settled when it
//! started.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    budgets::CodingTerminationReason,
    task_budget::{ledger_write_lock, write_atomic},
    CodingEngineEvent, CodingEngineEventKind,
};

const LEDGER_VERSION: u32 = 1;

/// Filename under the run's task directory, beside the active-time ledger.
pub const COST_LEDGER_FILE_NAME: &str = "coding_cost_ledger.json";

/// Charges past this count are folded into `compacted_micro_usd`, so a run with
/// hundreds of turns does not rewrite an ever-growing file on every turn.
const MAX_TRACKED_CHARGES: usize = 256;

/// USD to whole micro-dollars. Anything that is not a positive finite amount is
/// zero: a negative or NaN cost from a provider must never *refund* the budget.
pub fn usd_to_micro(usd: f64) -> u64 {
    if usd.is_finite() && usd > 0.0 {
        (usd * 1_000_000.0).round() as u64
    } else {
        0
    }
}

pub fn micro_to_usd(micro: u64) -> f64 {
    micro as f64 / 1_000_000.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CostCharge {
    execution_id: String,
    micro_usd: u64,
    at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CostLedgerFile {
    #[serde(default = "ledger_version")]
    version: u32,
    #[serde(default)]
    charges: Vec<CostCharge>,
    #[serde(default)]
    compacted_micro_usd: u64,
    /// Turns that finished without a dollar cost from their engine.
    #[serde(default)]
    unpriced_turns: u64,
}

fn ledger_version() -> u32 {
    LEDGER_VERSION
}

impl Default for CostLedgerFile {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            charges: Vec::new(),
            compacted_micro_usd: 0,
            unpriced_turns: 0,
        }
    }
}

impl CostLedgerFile {
    fn total(&self) -> u64 {
        self.charges
            .iter()
            .fold(self.compacted_micro_usd, |total, charge| {
                total.saturating_add(charge.micro_usd)
            })
    }

    fn compact(&mut self) {
        if self.charges.len() <= MAX_TRACKED_CHARGES {
            return;
        }
        let fold = self.charges.len() - MAX_TRACKED_CHARGES;
        let folded: u64 = self
            .charges
            .drain(..fold)
            .map(|charge| charge.micro_usd)
            .sum();
        self.compacted_micro_usd = self.compacted_micro_usd.saturating_add(folded);
    }
}

/// The durable spend record for one run. Like the active-time ledger it holds
/// no cached state: every read and write goes to the file.
pub struct CodingCostLedger {
    path: PathBuf,
}

impl CodingCostLedger {
    pub fn open(task_dir: &Path) -> Self {
        Self {
            path: task_dir.join(COST_LEDGER_FILE_NAME),
        }
    }

    /// An unreadable ledger starts fresh, as the active-time ledger does:
    /// refusing every future turn of a run over a corrupt JSON file would be
    /// worse than losing its history.
    fn load(&self) -> CostLedgerFile {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return CostLedgerFile::default();
        };
        match serde_json::from_slice::<CostLedgerFile>(&bytes) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    target: "coding_engine",
                    path = %self.path.display(),
                    %error,
                    "coding cost ledger is unreadable; starting a fresh one"
                );
                CostLedgerFile::default()
            },
        }
    }

    fn mutate(&self, apply: impl FnOnce(&mut CostLedgerFile)) {
        let _guard = match ledger_write_lock().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut state = self.load();
        apply(&mut state);
        state.compact();
        if let Err(error) = write_atomic(&self.path, &state) {
            tracing::warn!(
                target: "coding_engine",
                path = %self.path.display(),
                %error,
                "could not persist the coding cost ledger; spend will not survive a restart"
            );
        }
    }

    pub fn spent_micro_usd(&self) -> u64 {
        self.load().total()
    }

    pub fn unpriced_turns(&self) -> u64 {
        self.load().unpriced_turns
    }

    /// Record one finished turn. `None` is a turn whose engine reported no
    /// dollar cost.
    ///
    /// This records the run's own cap only. The owner's spend envelope is
    /// charged once per turn by `run_coding_task`, whether or not the run has
    /// a cap (and so a ledger at all).
    pub fn record_turn(&self, execution_id: &str, micro_usd: Option<u64>, now_ms: i64) {
        self.mutate(|state| match micro_usd {
            Some(micro_usd) => state.charges.push(CostCharge {
                execution_id: execution_id.to_string(),
                micro_usd,
                at_ms: now_ms,
            }),
            None => state.unpriced_turns = state.unpriced_turns.saturating_add(1),
        });
    }
}

// ---------------------------------------------------------------------------
// The live meter shared by a run's in-flight turns
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct RunSpendMeter {
    /// The ledger's total when the first live turn of this run opened the meter.
    base_micro_usd: u64,
    /// Turns that finished while the meter stayed open.
    settled_micro_usd: u64,
    /// Live spend per in-flight turn, by turn slot.
    live: HashMap<u64, u64>,
}

impl RunSpendMeter {
    fn total(&self) -> u64 {
        self.live.values().fold(
            self.base_micro_usd.saturating_add(self.settled_micro_usd),
            |total, live| total.saturating_add(*live),
        )
    }
}

type SharedMeter = Arc<Mutex<RunSpendMeter>>;

fn meters() -> &'static Mutex<HashMap<PathBuf, SharedMeter>> {
    static METERS: OnceLock<Mutex<HashMap<PathBuf, SharedMeter>>> = OnceLock::new();
    METERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// One turn's view of its run's cap. Created before the turn, settled after
/// it; dropping it unsettled keeps whatever live spend it saw, so an early
/// return can only over-count the run, never under-count it.
#[derive(Debug)]
pub struct CodingCostCeiling {
    ledger_path: PathBuf,
    limit_micro_usd: u64,
    slot: u64,
    meter: SharedMeter,
}

impl CodingCostCeiling {
    /// Join (or open) the run's meter. The ledger is read only when this is
    /// the run's first live turn in this process; while turns are live, the
    /// meter is the more current figure.
    pub fn begin(ledger: &CodingCostLedger, limit_micro_usd: u64) -> Arc<Self> {
        static NEXT_SLOT: AtomicU64 = AtomicU64::new(1);
        let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
        // The slot joins while the registry is still locked, so the last turn
        // closing the meter cannot remove it between this lookup and the join.
        let meter = {
            let mut registry = lock(meters());
            let meter = registry
                .entry(ledger.path.clone())
                .or_insert_with(|| {
                    Arc::new(Mutex::new(RunSpendMeter {
                        base_micro_usd: ledger.spent_micro_usd(),
                        ..RunSpendMeter::default()
                    }))
                })
                .clone();
            lock(&meter).live.insert(slot, 0);
            meter
        };
        Arc::new(Self {
            ledger_path: ledger.path.clone(),
            limit_micro_usd,
            slot,
            meter,
        })
    }

    pub fn limit_micro_usd(&self) -> u64 {
        self.limit_micro_usd
    }

    /// Everything this run has spent, including every live turn's spend so far.
    pub fn spent_micro_usd(&self) -> u64 {
        lock(&self.meter).total()
    }

    /// Add spend this turn learned about mid-turn.
    pub fn add_live(&self, micro_usd: u64) {
        if micro_usd == 0 {
            return;
        }
        let mut meter = lock(&self.meter);
        if let Some(live) = meter.live.get_mut(&self.slot) {
            *live = live.saturating_add(micro_usd);
        }
    }

    /// The typed stop, once the run's spend has reached the cap.
    pub fn exceeded(&self) -> Option<CodingTerminationReason> {
        let spent = self.spent_micro_usd();
        (spent >= self.limit_micro_usd).then_some(CodingTerminationReason::CostBudget {
            limit_micro_usd: self.limit_micro_usd,
            spent_micro_usd: spent,
        })
    }

    /// Replace this turn's live estimate with the engine's final figure, and
    /// return what the turn should be charged — `None` when neither the engine
    /// nor its events priced any of it.
    ///
    /// A final figure lower than the live one never wins: an engine that
    /// reported nothing (or less) at the end has not shown that it spent less
    /// than it reported along the way.
    pub fn settle(&self, final_micro_usd: Option<u64>) -> Option<u64> {
        let mut meter = lock(&self.meter);
        let live = meter.live.remove(&self.slot)?;
        let charged = match final_micro_usd {
            Some(reported) => Some(reported.max(live)),
            None => (live > 0).then_some(live),
        };
        meter.settled_micro_usd = meter.settled_micro_usd.saturating_add(charged.unwrap_or(0));
        charged
    }

    /// Pi stamps each finished assistant message with its own cost. That is
    /// the only per-message, non-cumulative cost any engine streams; the
    /// others carry a running total for the turn or session, and summing those
    /// would count the same spend many times over.
    pub fn observe(&self, event: &CodingEngineEvent) {
        if let Some(cost) = pi_assistant_message_cost(event) {
            self.add_live(usd_to_micro(cost));
        }
    }
}

impl Drop for CodingCostCeiling {
    fn drop(&mut self) {
        let _ = self.settle(None);
        let mut registry = lock(meters());
        let idle = lock(&self.meter).live.is_empty();
        // The last live turn closes the meter, so the next one rereads the
        // ledger — which by then holds every settled turn's charge.
        if idle
            && registry
                .get(&self.ledger_path)
                .is_some_and(|meter| Arc::ptr_eq(meter, &self.meter))
        {
            registry.remove(&self.ledger_path);
        }
    }
}

fn pi_assistant_message_cost(event: &CodingEngineEvent) -> Option<f64> {
    if event.kind != CodingEngineEventKind::MessageEnd
        || event.raw_type.as_deref() != Some("message_end")
        || event.raw.pointer("/message/role").and_then(Value::as_str) != Some("assistant")
    {
        return None;
    }
    event.usage.as_ref().and_then(|usage| usage.cost_total)
}

/// Budget fields for the run payload and the cockpit.
pub fn cost_telemetry(limit_micro_usd: u64, spent_micro_usd: u64, unpriced_turns: u64) -> Value {
    serde_json::json!({
        "limit_usd": micro_to_usd(limit_micro_usd),
        "spent_usd": micro_to_usd(spent_micro_usd),
        "remaining_usd": micro_to_usd(limit_micro_usd.saturating_sub(spent_micro_usd)),
        "unpriced_turns": unpriced_turns,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::execution::coding_engine::coding_event_from_raw;

    fn pi_message_end(role: &str, cost: f64) -> CodingEngineEvent {
        coding_event_from_raw(
            1,
            json!({
                "type": "message_end",
                "message": {
                    "role": role,
                    "usage": {"input": 10, "output": 5, "cost": {"total": cost}},
                },
            }),
        )
    }

    #[test]
    fn a_run_with_no_turns_has_spent_nothing() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        assert_eq!(ledger.spent_micro_usd(), 0);
        assert_eq!(ledger.unpriced_turns(), 0);
    }

    #[test]
    fn finished_turns_add_up_and_survive_a_reload() {
        let dir = TempDir::new().unwrap();
        CodingCostLedger::open(dir.path()).record_turn("exec-1", Some(1_250_000), 1);
        CodingCostLedger::open(dir.path()).record_turn("exec-2", Some(500_000), 2);
        CodingCostLedger::open(dir.path()).record_turn("exec-3", None, 3);
        let ledger = CodingCostLedger::open(dir.path());
        assert_eq!(ledger.spent_micro_usd(), 1_750_000);
        assert_eq!(ledger.unpriced_turns(), 1);
    }

    #[test]
    fn a_long_run_compacts_without_losing_spend() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        for turn in 0..(MAX_TRACKED_CHARGES as i64 + 10) {
            ledger.record_turn("exec", Some(10), turn);
        }
        assert_eq!(
            ledger.spent_micro_usd(),
            (MAX_TRACKED_CHARGES as u64 + 10) * 10
        );
        assert!(ledger.load().charges.len() <= MAX_TRACKED_CHARGES);
    }

    #[test]
    fn a_corrupt_ledger_starts_fresh() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(COST_LEDGER_FILE_NAME), b"{not json").unwrap();
        assert_eq!(CodingCostLedger::open(dir.path()).spent_micro_usd(), 0);
    }

    #[test]
    fn a_run_already_at_its_cap_is_refused_before_the_turn() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        ledger.record_turn("exec-1", Some(2_000_000), 1);
        let ceiling = CodingCostCeiling::begin(&ledger, 2_000_000);
        assert_eq!(
            ceiling.exceeded(),
            Some(CodingTerminationReason::CostBudget {
                limit_micro_usd: 2_000_000,
                spent_micro_usd: 2_000_000,
            })
        );
    }

    #[test]
    fn pi_message_costs_stop_the_turn_that_crosses_the_cap() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        ledger.record_turn("exec-1", Some(900_000), 1);
        let ceiling = CodingCostCeiling::begin(&ledger, 1_000_000);
        ceiling.observe(&pi_message_end("assistant", 0.06));
        assert!(ceiling.exceeded().is_none());
        ceiling.observe(&pi_message_end("assistant", 0.05));
        assert!(matches!(
            ceiling.exceeded(),
            Some(CodingTerminationReason::CostBudget {
                spent_micro_usd: 1_010_000,
                ..
            })
        ));
    }

    #[test]
    fn only_pi_assistant_messages_are_counted_mid_turn() {
        let dir = TempDir::new().unwrap();
        let ceiling = CodingCostCeiling::begin(&CodingCostLedger::open(dir.path()), 1_000_000);
        ceiling.observe(&pi_message_end("user", 5.0));
        // A running total on another event shape (turn_end, message_update)
        // repeats spend already counted; it must not be added again.
        ceiling.observe(&coding_event_from_raw(
            2,
            json!({
                "type": "turn_end",
                "message": {"role": "assistant", "usage": {"cost": {"total": 5.0}}},
            }),
        ));
        assert_eq!(ceiling.spent_micro_usd(), 0);
    }

    #[test]
    fn parallel_turns_of_one_run_see_each_others_live_spend() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        let first = CodingCostCeiling::begin(&ledger, 1_000_000);
        let second = CodingCostCeiling::begin(&ledger, 1_000_000);
        first.add_live(600_000);
        assert!(second.exceeded().is_none());
        second.add_live(400_000);
        assert!(first.exceeded().is_some());
        assert!(second.exceeded().is_some());
    }

    #[test]
    fn a_settled_sibling_still_counts_while_the_other_turn_runs() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        let first = CodingCostCeiling::begin(&ledger, 1_000_000);
        let second = CodingCostCeiling::begin(&ledger, 1_000_000);
        first.add_live(300_000);
        assert_eq!(first.settle(Some(700_000)), Some(700_000));
        ledger.record_turn("exec-1", Some(700_000), 1);
        drop(first);
        second.add_live(300_000);
        assert_eq!(second.spent_micro_usd(), 1_000_000);
        assert!(second.exceeded().is_some());
    }

    #[test]
    fn the_next_run_turn_rereads_the_ledger_once_the_meter_is_idle() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        let turn = CodingCostCeiling::begin(&ledger, 5_000_000);
        turn.add_live(100_000);
        let _ = turn.settle(Some(1_000_000));
        ledger.record_turn("exec-1", Some(1_000_000), 1);
        drop(turn);
        let next = CodingCostCeiling::begin(&ledger, 5_000_000);
        assert_eq!(next.spent_micro_usd(), 1_000_000);
    }

    #[test]
    fn a_lower_final_figure_never_undoes_spend_seen_live() {
        let dir = TempDir::new().unwrap();
        let ledger = CodingCostLedger::open(dir.path());
        let first = CodingCostCeiling::begin(&ledger, 10_000_000);
        let second = CodingCostCeiling::begin(&ledger, 10_000_000);
        first.add_live(800_000);
        assert_eq!(first.settle(Some(500_000)), Some(800_000));
        assert_eq!(second.spent_micro_usd(), 800_000);
        // Nothing priced at all is unpriced, not a $0.00 charge.
        assert_eq!(second.settle(None), None);
    }

    #[test]
    fn a_bad_provider_cost_never_refunds_the_budget() {
        assert_eq!(usd_to_micro(-1.0), 0);
        assert_eq!(usd_to_micro(f64::NAN), 0);
        assert_eq!(usd_to_micro(f64::INFINITY), 0);
        assert_eq!(usd_to_micro(0.25), 250_000);
        assert_eq!(usd_to_micro(0.000_000_4), 0);
    }
}
