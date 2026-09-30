//! Company-loop cycle-outcome smoke: the outcome-classification layer must NOT
//! turn a benign "no work" / healthy cycle into a `CycleFailed` / `CycleDropped`
//! anomaly. This is the pure-ish classifier guard for reliability audit gap #9
//! ("Company-loop cycle smoke") — a healthy cycle leaves no open anomaly, and an
//! intentional/benign drop (disabled/paused/duplicate) is not surfaced as a
//! reliability anomaly.
//!
//! We drive `classify_cycle_anomaly` through the SAME wire strings the recorder
//! emits (`HarnessCycleDispatchOutcome::as_str()`), rather than booting the
//! orchestrator (which needs a live LLM). The fuller end-to-end CEO/standup cycle
//! smoke is captured as an `#[ignore]`d spec below.

use magician::magician_v2::harness::anomaly::{classify_cycle_anomaly, AnomalyKind};
use magician::magician_v2::harness::cycle_outcome::HarnessCycleDispatchOutcome;

/// A healthy cycle that persisted an episode with no failure produces NO anomaly.
/// This is the trivial-goal "reached a terminal outcome, nothing to fix" case:
/// the classifier must return `None`, so the reliability layer leaves nothing
/// open for the (agent, goal).
#[test]
fn healthy_persisted_cycle_leaves_no_open_anomaly() {
    let outcome = HarnessCycleDispatchOutcome::EpisodePersisted;
    let result = classify_cycle_anomaly(
        outcome.as_str(),
        /* episode_failed = */ false,
        /* detail = */ "",
    );
    assert!(
        result.is_none(),
        "a healthy persisted cycle must not surface any anomaly, got {result:?}"
    );
}

/// A benign "no work" cycle that never ran the agent because it was intentionally
/// disabled/paused, or deduped as an idempotent duplicate, must NOT be classified
/// as `CycleFailed` or `CycleDropped`. These are drops, but benign ones — the
/// reliability layer should stay silent.
#[test]
fn benign_no_work_drops_are_not_anomalies() {
    for outcome in [
        HarnessCycleDispatchOutcome::DroppedDisabled,
        HarnessCycleDispatchOutcome::DroppedPaused,
        HarnessCycleDispatchOutcome::DroppedDuplicate,
        HarnessCycleDispatchOutcome::DroppedReservationChanged,
    ] {
        let result = classify_cycle_anomaly(outcome.as_str(), false, "");
        assert!(
            result.is_none(),
            "benign no-work drop {outcome:?} must not surface an anomaly, got {result:?}"
        );
    }
}

/// Guard rail (asymmetry check): a genuinely failed cycle DOES surface an anomaly,
/// so the "healthy → None" result above is meaningful and not a classifier that
/// always returns `None`. A provision-failed drop is a real `CycleDropped`; a
/// failed episode is a real `CycleFailed`.
#[test]
fn genuinely_broken_cycles_still_surface_an_anomaly() {
    let dropped = classify_cycle_anomaly(
        HarnessCycleDispatchOutcome::DroppedProvisionFailed.as_str(),
        false,
        "",
    );
    assert_eq!(
        dropped.map(|(k, _)| k),
        Some(AnomalyKind::CycleDropped),
        "a provision-failed drop must surface CycleDropped"
    );

    let failed = classify_cycle_anomaly(
        HarnessCycleDispatchOutcome::EpisodePersistFailed.as_str(),
        true,
        "boom",
    );
    assert_eq!(
        failed.map(|(k, _)| k),
        Some(AnomalyKind::CycleFailed),
        "an unattributed failed episode must fall back to the generic CycleFailed"
    );
}

/// Fuller company-loop smoke: run one real CEO/standup harness cycle for a trivial
/// goal end-to-end (episode persisted, no open `CycleFailed`, no permit
/// starvation). This requires booting the orchestrator + a live LLM, which is not
/// available in the unit-test harness, so it is ignored here; the classifier-layer
/// tests above guard the specific "benign cycle is not a failure" fix without it.
#[test]
#[ignore = "needs a live orchestrator + LLM to run a real harness cycle; the classifier-layer tests guard the fix without booting the process"]
fn company_loop_trivial_goal_reaches_terminal_outcome() {
    // Intended assertion once infrastructure exists: dispatch one harness cycle
    // for a trivial goal, then assert the recorded outcome is
    // `HarnessCycleDispatchOutcome::EpisodePersisted` (not a `Dropped*`), and that
    // `classify_cycle_anomaly` over that outcome yields `None` (no open
    // CycleFailed/CycleDropped for the (agent, goal)).
    let outcome = HarnessCycleDispatchOutcome::EpisodePersisted;
    assert_eq!(outcome.as_str(), "episode_persisted");
    assert!(classify_cycle_anomaly(outcome.as_str(), false, "").is_none());
}
