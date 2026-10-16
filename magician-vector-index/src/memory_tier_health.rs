//! Effectiveness metrics for the memory *temperature tiering*, as opposed to
//! memory retrieval.
//!
//! The existing memory evals all ask a retrieval question: given this query,
//! did the renderer surface the durable fact? They measure recall, MRR, hybrid
//! backend coverage, and latency. None of them asks whether the tier a memory
//! was placed in is correct or useful, which is why two structural defects
//! (unearned active-tier promotion in one-entry lane partitions, and unbounded
//! overlay growth) survived in production data while every retrieval gate
//! stayed green.
//!
//! Everything here is computed from an overlay alone: no provider, no network,
//! no index. That keeps it cheap enough to run as a unit test, against a live
//! overlay read-only, and inside the cost-bearing live eval.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::memory_candidates::SemanticMemoryType;
use crate::memory_temperature::{
    memory_temperature_candidate_key_is_current, memory_temperature_entry_is_superseded,
    memory_temperature_scope_partition_for_health, MemoryTemperatureEntry,
    MemoryTemperatureOverlay, MemoryTemperatureTier,
};

/// Per-lane slice of the tier distribution.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryTierLaneHealth {
    pub entries: usize,
    pub active: usize,
    pub unearned_active: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryTierHealthMetrics {
    pub total_entries: usize,
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub t3: usize,

    /// Share of the overlay occupying the hot/active working set.
    pub working_set_ratio: f64,
    /// Entries in T0/T1 with no retrieval, selection, injection, outcome, or
    /// review signal — promoted by score and lane capacity alone.
    pub unearned_active_count: usize,
    /// `unearned_active_count` as a share of T0+T1. This is the headline tier
    /// correctness number: an active tier is supposed to be earned.
    pub unearned_active_ratio: f64,

    /// P(selected | T0 or T1) ÷ P(selected | T2 or T3).
    ///
    /// The discrimination the tier actually buys. 1.0 means the tier tells you
    /// nothing about whether a memory gets used; `None` means one side had no
    /// entries to compare.
    pub tier_lift: Option<f64>,
    pub active_selected_rate: f64,
    pub cold_selected_rate: f64,

    /// Share of entries with no signal of any kind — the compaction target.
    pub dead_entry_ratio: f64,
    pub dead_entry_count: usize,

    /// Entries still carrying the pre-v6 ambiguous key encoding.
    ///
    /// Reported, not gated. It cannot reach zero by correct behaviour: an entry
    /// that carries usage signal but whose candidate is gone is kept as
    /// evidence, and migration is candidate-driven, so there is nothing left to
    /// derive a new key from. Its historical key *is* the historical record.
    pub legacy_key_count: usize,
    pub legacy_key_ratio: f64,
    /// Legacy-encoded entries that DO still have a live candidate — memory that
    /// can migrate and has not. This is the real migration-completeness gate,
    /// and it must reach zero. `None` when the caller did not supply the live
    /// candidate set, in which case the gate reports `skipped` rather than
    /// inventing a verdict.
    pub unmigrated_live_count: Option<usize>,
    pub unmigrated_live_ratio: Option<f64>,
    pub max_key_chars: usize,

    pub partition_count: usize,
    /// Share of entries alone in their `(scope, agent, goal)` partition. High
    /// fragmentation means lane capacity stops bounding anything, because
    /// `percentage_cap` ceilings every non-empty lane to at least one slot.
    pub singleton_partition_ratio: f64,
    pub singleton_partition_entries: usize,

    pub superseded_count: usize,
    /// Superseded entries not parked in T3. Invariant: must be zero.
    pub superseded_active_count: usize,

    pub lanes: BTreeMap<String, MemoryTierLaneHealth>,
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    numerator as f64 / denominator as f64
}

/// Whether an entry carries any durable evidence of having been used.
///
/// Mirrors the runtime's own admission rule for the active tier. Kept as a
/// separate public predicate so the metric measures the *observable* property
/// ("has this memory ever participated in a turn?") rather than re-deriving a
/// private runtime decision, and so a future change to the runtime rule shows
/// up as a metric movement instead of silently redefining the metric.
pub fn entry_has_signal(entry: &MemoryTemperatureEntry) -> bool {
    entry.retrieved_count > 0
        || entry.selected_count > 0
        || entry.injected_count > 0
        || entry.successful_use_count > 0
        || entry.failed_use_count > 0
        || entry.reviewed_referenced_count > 0
        || entry.reviewed_useful_count > 0
        || entry.reviewed_load_bearing_count > 0
        || entry.reviewed_irrelevant_count > 0
        || entry.reviewed_stale_count > 0
        || entry.reviewed_harmful_count > 0
        || entry.last_utility_review_at.is_some()
        || !entry.supersedes.is_empty()
}

fn is_active(tier: MemoryTemperatureTier) -> bool {
    matches!(tier, MemoryTemperatureTier::T0 | MemoryTemperatureTier::T1)
}

/// Lane keys are a closed set of ten `&'static str`s. Allocating a `String`
/// per entry to look one up meant thousands of allocations immediately dropped
/// by `or_default` finding the existing key.
fn lane_label(semantic_memory_type: SemanticMemoryType) -> &'static str {
    semantic_memory_type.as_str()
}

pub fn compute_memory_tier_health(overlay: &MemoryTemperatureOverlay) -> MemoryTierHealthMetrics {
    compute_memory_tier_health_with_live_keys(overlay, None)
}

/// As [`compute_memory_tier_health`], but able to distinguish memory that can
/// still migrate from memory that is only kept as evidence.
pub fn compute_memory_tier_health_with_live_keys(
    overlay: &MemoryTemperatureOverlay,
    live_candidate_keys: Option<&BTreeSet<String>>,
) -> MemoryTierHealthMetrics {
    let mut metrics = MemoryTierHealthMetrics {
        total_entries: overlay.entries.len(),
        ..Default::default()
    };

    // Resolve each key's partition once; the overlay is large enough in
    // practice (thousands of entries) that recomputing it per lookup is
    // needless allocation.
    let partitions = overlay
        .entries
        .keys()
        .map(|key| {
            (
                key.as_str(),
                memory_temperature_scope_partition_for_health(key),
            )
        })
        .collect::<BTreeMap<&str, Cow<'_, str>>>();
    let mut partition_sizes: BTreeMap<&str, usize> = BTreeMap::new();
    for partition in partitions.values() {
        *partition_sizes.entry(partition.as_ref()).or_default() += 1;
    }
    metrics.partition_count = partition_sizes.len();

    // Accumulated against static lane names and converted once at the end, so
    // the ten-key map costs ten allocations rather than one per entry.
    let mut lanes: BTreeMap<&'static str, MemoryTierLaneHealth> = BTreeMap::new();
    let mut unmigrated_live = 0usize;
    let mut live_entries = 0usize;
    let mut active_total = 0usize;
    let mut active_selected = 0usize;
    let mut cold_total = 0usize;
    let mut cold_selected = 0usize;

    for (key, entry) in &overlay.entries {
        let tier = entry.temperature_tier;
        match tier {
            MemoryTemperatureTier::T0 => metrics.t0 += 1,
            MemoryTemperatureTier::T1 => metrics.t1 += 1,
            MemoryTemperatureTier::T2 => metrics.t2 += 1,
            MemoryTemperatureTier::T3 => metrics.t3 += 1,
        }

        let signal = entry_has_signal(entry);
        if !signal {
            metrics.dead_entry_count += 1;
        }
        let is_live = live_candidate_keys.is_some_and(|live| live.contains(key));
        if is_live {
            live_entries += 1;
        }
        if !memory_temperature_candidate_key_is_current(key) {
            metrics.legacy_key_count += 1;
            if is_live {
                unmigrated_live += 1;
            }
        }
        metrics.max_key_chars = metrics.max_key_chars.max(key.chars().count());

        let alone_in_partition = partitions
            .get(key.as_str())
            .and_then(|partition| partition_sizes.get(partition.as_ref()))
            .copied()
            .unwrap_or(0)
            == 1;
        if alone_in_partition {
            metrics.singleton_partition_entries += 1;
        }

        if memory_temperature_entry_is_superseded(entry) {
            metrics.superseded_count += 1;
            if tier != MemoryTemperatureTier::T3 {
                metrics.superseded_active_count += 1;
            }
        }

        let lane = lanes
            .entry(lane_label(entry.semantic_memory_type))
            .or_default();
        lane.entries += 1;

        let selected = entry.selected_count > 0;
        if is_active(tier) {
            active_total += 1;
            lane.active += 1;
            if !signal {
                metrics.unearned_active_count += 1;
                lane.unearned_active += 1;
            }
            if selected {
                active_selected += 1;
            }
        } else {
            cold_total += 1;
            if selected {
                cold_selected += 1;
            }
        }
    }

    metrics.lanes = lanes
        .into_iter()
        .map(|(lane, health)| (lane.to_string(), health))
        .collect();
    metrics.working_set_ratio = ratio(active_total, metrics.total_entries);
    metrics.unearned_active_ratio = ratio(metrics.unearned_active_count, active_total);
    metrics.dead_entry_ratio = ratio(metrics.dead_entry_count, metrics.total_entries);
    metrics.legacy_key_ratio = ratio(metrics.legacy_key_count, metrics.total_entries);
    // Only claim a verdict when the live set actually intersects the overlay.
    // A caller supplying current-format keys against a pre-migration overlay
    // has zero overlap, and `ratio(0, 0)` is 0.0 — which would report "0%
    // unmigrated, passed" about an overlay that is 100% unmigrated. Reporting
    // `None` makes the gate skip, which is the honest answer.
    if live_candidate_keys.is_some() && live_entries > 0 {
        metrics.unmigrated_live_count = Some(unmigrated_live);
        metrics.unmigrated_live_ratio = Some(ratio(unmigrated_live, live_entries));
    }
    metrics.singleton_partition_ratio =
        ratio(metrics.singleton_partition_entries, metrics.total_entries);
    metrics.active_selected_rate = ratio(active_selected, active_total);
    metrics.cold_selected_rate = ratio(cold_selected, cold_total);
    metrics.tier_lift = if active_total == 0 || cold_total == 0 || metrics.cold_selected_rate == 0.0
    {
        None
    } else {
        Some(metrics.active_selected_rate / metrics.cold_selected_rate)
    };

    metrics
}

/// Thresholds the tier layer is expected to hold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MemoryTierHealthGates {
    /// The active tier must be overwhelmingly earned.
    pub max_unearned_active_ratio: f64,
    /// The working set is a working set, not the whole overlay.
    pub max_working_set_ratio: f64,
    /// Tiering must beat a coin flip at predicting use.
    pub min_tier_lift: f64,
    /// Bound on accumulated no-signal entries.
    pub max_dead_entry_ratio: f64,
    /// Migration completeness, over memory that can actually migrate.
    pub max_unmigrated_live_ratio: f64,
    /// Keys are identifiers, not payloads.
    pub max_key_chars: usize,
}

impl Default for MemoryTierHealthGates {
    fn default() -> Self {
        Self {
            max_unearned_active_ratio: 0.05,
            max_working_set_ratio: 0.35,
            min_tier_lift: 1.5,
            max_dead_entry_ratio: 0.50,
            max_unmigrated_live_ratio: 0.0,
            max_key_chars: 1_024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryTierHealthGateResult {
    pub name: String,
    pub actual: f64,
    pub threshold: f64,
    /// `true` when the gate is an upper bound, `false` when a lower bound.
    pub upper_bound: bool,
    pub passed: bool,
    /// Set when the metric could not be computed rather than failing.
    pub skipped: bool,
}

fn upper_gate(name: &str, actual: f64, threshold: f64) -> MemoryTierHealthGateResult {
    MemoryTierHealthGateResult {
        name: name.to_string(),
        actual,
        threshold,
        upper_bound: true,
        passed: actual <= threshold,
        skipped: false,
    }
}

/// An upper-bound gate whose metric may be unavailable. An absent metric is
/// reported `skipped`, never silently passed or failed.
fn upper_gate_optional(
    name: &str,
    actual: Option<f64>,
    threshold: f64,
) -> MemoryTierHealthGateResult {
    match actual {
        Some(actual) => upper_gate(name, actual, threshold),
        None => MemoryTierHealthGateResult {
            name: name.to_string(),
            actual: 0.0,
            threshold,
            upper_bound: true,
            passed: true,
            skipped: true,
        },
    }
}

fn lower_gate(name: &str, actual: Option<f64>, threshold: f64) -> MemoryTierHealthGateResult {
    match actual {
        Some(actual) => MemoryTierHealthGateResult {
            name: name.to_string(),
            actual,
            threshold,
            upper_bound: false,
            passed: actual >= threshold,
            skipped: false,
        },
        None => MemoryTierHealthGateResult {
            name: name.to_string(),
            actual: 0.0,
            threshold,
            upper_bound: false,
            // Not enough population on one side to compare. Reported, never
            // silently counted as a pass or a failure.
            passed: true,
            skipped: true,
        },
    }
}

pub fn evaluate_memory_tier_health(
    metrics: &MemoryTierHealthMetrics,
    gates: MemoryTierHealthGates,
) -> Vec<MemoryTierHealthGateResult> {
    vec![
        upper_gate(
            "unearned active ratio",
            metrics.unearned_active_ratio,
            gates.max_unearned_active_ratio,
        ),
        upper_gate(
            "working set ratio",
            metrics.working_set_ratio,
            gates.max_working_set_ratio,
        ),
        lower_gate("tier lift", metrics.tier_lift, gates.min_tier_lift),
        upper_gate(
            "dead entry ratio",
            metrics.dead_entry_ratio,
            gates.max_dead_entry_ratio,
        ),
        // Gated on the population that can migrate, not on every legacy key.
        // Evidence-only entries keep their historical key by design.
        upper_gate_optional(
            "unmigrated live memory",
            metrics.unmigrated_live_ratio,
            gates.max_unmigrated_live_ratio,
        ),
        upper_gate(
            "max key chars",
            metrics.max_key_chars as f64,
            gates.max_key_chars as f64,
        ),
        upper_gate(
            "superseded entries left active",
            metrics.superseded_active_count as f64,
            0.0,
        ),
    ]
}

/// Distinct partitions represented in an overlay. Exposed for diagnostics that
/// want the fragmentation shape rather than the single ratio.
pub fn memory_tier_partitions(overlay: &MemoryTemperatureOverlay) -> BTreeSet<String> {
    overlay
        .entries
        .keys()
        .map(|key| memory_temperature_scope_partition_for_health(key).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_temperature::{default_temperature_score, MemoryTemperatureEntry};
    use chrono::{TimeZone, Utc};

    fn entry(
        key: &str,
        semantic_memory_type: SemanticMemoryType,
        tier: MemoryTemperatureTier,
        selected_count: u32,
    ) -> MemoryTemperatureEntry {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap();
        MemoryTemperatureEntry {
            memory_candidate_key: key.to_string(),
            semantic_memory_type,
            temperature_tier: tier,
            temperature_score: default_temperature_score(semantic_memory_type),
            confidence: None,
            last_retrieved_at: None,
            last_selected_at: None,
            last_injected_at: None,
            last_used_at: None,
            retrieved_count: 0,
            selected_count,
            injected_count: 0,
            successful_use_count: 0,
            failed_use_count: 0,
            reviewed_referenced_count: 0,
            reviewed_useful_count: 0,
            reviewed_load_bearing_count: 0,
            reviewed_irrelevant_count: 0,
            reviewed_stale_count: 0,
            reviewed_harmful_count: 0,
            source_ids: Vec::new(),
            superseded_by: None,
            superseded_at: None,
            supersession_reason: None,
            supersession_confidence: None,
            supersession_source: None,
            supersedes: Vec::new(),
            first_seen_at: Some(now),
            last_temperature_review_at: None,
            last_temperature_change_reason: None,
            last_utility_review_at: None,
            last_utility_review_run_id: None,
            last_utility_review_label: None,
            last_utility_review_confidence: None,
            last_utility_review_reason: None,
            updated_at: now,
        }
    }

    fn overlay_from(entries: Vec<(String, MemoryTemperatureEntry)>) -> MemoryTemperatureOverlay {
        let mut overlay = MemoryTemperatureOverlay::default();
        for (key, entry) in entries {
            overlay.entries.insert(key, entry);
        }
        overlay
    }

    fn agent_goal_key(goal: &str) -> String {
        format!(
            "mt1:10:agent_goal:18:personal-assistant:{}:{goal}:19:task_progress.notes:1:0",
            goal.len()
        )
    }

    /// The exact production shape: one never-used `project_context` entry per
    /// task goal, each parked in T1. The unearned-active gate must fail on it —
    /// this is the eval that would have caught the defect.
    #[test]
    fn unearned_active_gate_fails_on_the_production_shape() {
        let entries = (0..64)
            .map(|index| {
                let key = agent_goal_key(&format!("task_{index:032x}"));
                (
                    key.clone(),
                    entry(
                        &key,
                        SemanticMemoryType::ProjectContext,
                        MemoryTemperatureTier::T1,
                        0,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let metrics = compute_memory_tier_health(&overlay_from(entries));

        assert_eq!(metrics.t1, 64);
        assert_eq!(metrics.unearned_active_count, 64);
        assert_eq!(metrics.unearned_active_ratio, 1.0);
        assert_eq!(metrics.singleton_partition_ratio, 1.0);
        assert_eq!(metrics.dead_entry_ratio, 1.0);

        let gates = evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default());
        let failed = gates
            .iter()
            .filter(|gate| !gate.passed && !gate.skipped)
            .map(|gate| gate.name.as_str())
            .collect::<Vec<_>>();
        assert!(
            failed.contains(&"unearned active ratio"),
            "failed: {failed:?}"
        );
        assert!(failed.contains(&"working set ratio"), "failed: {failed:?}");
    }

    /// The same overlay after maintenance demotes unearned entries: every gate
    /// that measures tier correctness passes.
    #[test]
    fn a_healthy_overlay_passes_every_tier_gate() {
        let mut entries = Vec::new();
        for index in 0..8 {
            let key = agent_goal_key(&format!("hot_{index:032x}"));
            let mut active = entry(
                &key,
                SemanticMemoryType::ProjectContext,
                MemoryTemperatureTier::T1,
                3,
            );
            active.retrieved_count = 5;
            active.injected_count = 3;
            entries.push((key, active));
        }
        for index in 0..40 {
            let key = agent_goal_key(&format!("cold_{index:032x}"));
            let mut cold = entry(
                &key,
                SemanticMemoryType::ProjectContext,
                MemoryTemperatureTier::T2,
                0,
            );
            cold.retrieved_count = 1;
            entries.push((key, cold));
        }
        let metrics = compute_memory_tier_health(&overlay_from(entries));

        assert_eq!(metrics.unearned_active_count, 0);
        assert!(
            metrics.working_set_ratio < 0.35,
            "{}",
            metrics.working_set_ratio
        );
        assert_eq!(metrics.legacy_key_count, 0);

        for gate in evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default()) {
            assert!(
                gate.passed,
                "gate `{}` failed: {} vs {}",
                gate.name, gate.actual, gate.threshold
            );
        }
    }

    #[test]
    fn tier_lift_measures_whether_the_tier_predicts_use() {
        // Active entries all selected, cold entries half selected -> 2x lift.
        let mut entries = Vec::new();
        for index in 0..10 {
            let key = agent_goal_key(&format!("a_{index:032x}"));
            entries.push((
                key.clone(),
                entry(
                    &key,
                    SemanticMemoryType::Procedure,
                    MemoryTemperatureTier::T1,
                    1,
                ),
            ));
        }
        for index in 0..10 {
            let key = agent_goal_key(&format!("c_{index:032x}"));
            entries.push((
                key.clone(),
                entry(
                    &key,
                    SemanticMemoryType::Procedure,
                    MemoryTemperatureTier::T2,
                    u32::from(index % 2 == 0),
                ),
            ));
        }
        let metrics = compute_memory_tier_health(&overlay_from(entries));
        assert_eq!(metrics.tier_lift, Some(2.0));
    }

    #[test]
    fn tier_lift_is_skipped_rather_than_scored_when_one_side_is_empty() {
        let key = agent_goal_key("only");
        let metrics = compute_memory_tier_health(&overlay_from(vec![(
            key.clone(),
            entry(
                &key,
                SemanticMemoryType::Procedure,
                MemoryTemperatureTier::T1,
                1,
            ),
        )]));
        assert_eq!(metrics.tier_lift, None);

        let lift = evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default())
            .into_iter()
            .find(|gate| gate.name == "tier lift")
            .expect("tier lift gate");
        assert!(lift.skipped);
        assert!(lift.passed, "a skipped gate must not fail the run");
    }

    #[test]
    fn legacy_key_gate_catches_an_unmigrated_overlay() {
        let legacy = "agent_goal:personal-assistant:task_1:task_progress.notes:0".to_string();
        let metrics = compute_memory_tier_health(&overlay_from(vec![(
            legacy.clone(),
            entry(
                &legacy,
                SemanticMemoryType::ProjectContext,
                MemoryTemperatureTier::T2,
                0,
            ),
        )]));
        assert_eq!(metrics.legacy_key_count, 1);
        assert_eq!(metrics.legacy_key_ratio, 1.0);

        // Without the live candidate set there is no way to tell "not migrated
        // yet" from "kept as evidence under its historical key", so the gate
        // must abstain rather than guess.
        let gate = evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default())
            .into_iter()
            .find(|gate| gate.name == "unmigrated live memory")
            .expect("migration gate");
        assert!(gate.skipped);
    }

    #[test]
    fn migration_gate_fails_only_for_memory_that_could_have_migrated() {
        let legacy_live = "agent_goal:personal-assistant:task_1:task_progress.notes:0".to_string();
        let legacy_orphan = "agent:cto::design_decisions.entries:9".to_string();
        let overlay = overlay_from(vec![
            (
                legacy_live.clone(),
                entry(
                    &legacy_live,
                    SemanticMemoryType::ProjectContext,
                    MemoryTemperatureTier::T2,
                    0,
                ),
            ),
            (
                legacy_orphan.clone(),
                entry(
                    &legacy_orphan,
                    SemanticMemoryType::ProjectContext,
                    MemoryTemperatureTier::T2,
                    4,
                ),
            ),
        ]);

        // Only the first still has a candidate behind it.
        let live = [legacy_live].into_iter().collect::<BTreeSet<_>>();
        let metrics = compute_memory_tier_health_with_live_keys(&overlay, Some(&live));
        assert_eq!(metrics.legacy_key_count, 2, "both keys are legacy-encoded");
        assert_eq!(
            metrics.unmigrated_live_count,
            Some(1),
            "only the one with a live candidate is migratable debt"
        );

        let gate = evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default())
            .into_iter()
            .find(|gate| gate.name == "unmigrated live memory")
            .expect("migration gate");
        assert!(!gate.passed);
        assert!(!gate.skipped);
    }

    #[test]
    fn evidence_kept_under_a_historical_key_is_not_migration_debt() {
        // A migrated live entry alongside an evidence record whose candidate is
        // long gone. The orphan is legacy-keyed but cannot migrate, so it is not
        // debt; the live entry is what makes the ratio meaningful at all.
        let live_key = "mt1:5:agent:3:cto:0::4:tier:1:0".to_string();
        let legacy_orphan = "agent:cto::design_decisions.entries:9".to_string();
        let overlay = overlay_from(vec![
            (
                live_key.clone(),
                entry(
                    &live_key,
                    SemanticMemoryType::ProjectContext,
                    MemoryTemperatureTier::T2,
                    1,
                ),
            ),
            (
                legacy_orphan.clone(),
                entry(
                    &legacy_orphan,
                    SemanticMemoryType::ProjectContext,
                    MemoryTemperatureTier::T2,
                    4,
                ),
            ),
        ]);
        let live = [live_key].into_iter().collect::<BTreeSet<_>>();
        let metrics = compute_memory_tier_health_with_live_keys(&overlay, Some(&live));

        assert_eq!(metrics.legacy_key_count, 1, "the orphan is legacy-keyed");
        assert_eq!(
            metrics.unmigrated_live_count,
            Some(0),
            "but it has no live candidate, so it is not migration debt"
        );
        let gate = evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default())
            .into_iter()
            .find(|gate| gate.name == "unmigrated live memory")
            .expect("migration gate");
        assert!(gate.passed && !gate.skipped);
    }

    /// A live set that shares no key with the overlay cannot distinguish
    /// "nothing to migrate" from "you handed me the wrong keys" — and
    /// `ratio(0, 0)` is 0.0, which would report a clean pass on an overlay that
    /// is entirely unmigrated. The gate must abstain instead.
    #[test]
    fn a_live_set_with_no_overlap_abstains_rather_than_reporting_clean() {
        let legacy = "agent:cto::design_decisions.entries:9".to_string();
        let overlay = overlay_from(vec![(
            legacy.clone(),
            entry(
                &legacy,
                SemanticMemoryType::ProjectContext,
                MemoryTemperatureTier::T2,
                0,
            ),
        )]);
        // Current-format keys against a pre-migration overlay: zero overlap.
        let live = ["mt1:5:agent:3:cto:0::4:tier:1:0".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let metrics = compute_memory_tier_health_with_live_keys(&overlay, Some(&live));

        assert_eq!(metrics.legacy_key_ratio, 1.0, "100% unmigrated in truth");
        assert_eq!(metrics.unmigrated_live_ratio, None);
        let gate = evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default())
            .into_iter()
            .find(|gate| gate.name == "unmigrated live memory")
            .expect("migration gate");
        assert!(gate.skipped, "must not claim a clean pass here");
    }

    #[test]
    fn superseded_memory_left_outside_t3_is_an_invariant_violation() {
        let key = agent_goal_key("stale");
        let mut stale = entry(
            &key,
            SemanticMemoryType::ProjectContext,
            MemoryTemperatureTier::T1,
            1,
        );
        stale.superseded_by = Some(agent_goal_key("fresh"));
        let metrics = compute_memory_tier_health(&overlay_from(vec![(key, stale)]));

        assert_eq!(metrics.superseded_count, 1);
        assert_eq!(metrics.superseded_active_count, 1);
        let gate = evaluate_memory_tier_health(&metrics, MemoryTierHealthGates::default())
            .into_iter()
            .find(|gate| gate.name == "superseded entries left active")
            .expect("supersession gate");
        assert!(!gate.passed);
    }

    #[test]
    fn an_empty_overlay_produces_no_division_by_zero() {
        let metrics = compute_memory_tier_health(&MemoryTemperatureOverlay::default());
        assert_eq!(metrics.total_entries, 0);
        assert_eq!(metrics.working_set_ratio, 0.0);
        assert_eq!(metrics.unearned_active_ratio, 0.0);
        assert_eq!(metrics.tier_lift, None);
        assert!(memory_tier_partitions(&MemoryTemperatureOverlay::default()).is_empty());
    }
}
