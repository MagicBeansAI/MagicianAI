//! Recurring Monitors Phase 6 — PROVIDER-FREE golden evaluation set
//! (plan §13 Phase 6 item 4; scenario list from the plan: public page price
//! change, authenticated dashboard with auth_failed, release-notes new
//! entry, date/status flip, transient failure then recovery, unchanged
//! page, cosmetic-only change).
//!
//! Each scenario under `tests/fixtures/monitors/golden/*.json` is a small
//! script of model-CLAIMED `MonitorRunResultV1` payloads plus the expected
//! backend verdicts. The harness drives the PURE Phase 2/3 semantics the
//! service uses — `validate_monitor_run_result` → `compare_runs` →
//! `finalize_run_result` → `would_notify` → `build_monitor_update` /
//! `update_projects_to_changed` — chaining the hot cursor between steps
//! exactly the way `ArtifactV2Service::accept_monitor_run` does (see the
//! non-failed arm in `artifact_v2/service.rs`; kept in sync by comment
//! reference, and the service-level tests in `monitor_run.rs` cover the
//! real persistence path).
//!
//! Deterministic by design: no LLM, no network, no clock — the model's
//! classification/status/fingerprint claims are inputs, the backend verdict
//! is the unit under eval. The LIVE run-quality lane (real executions
//! against real pages through a running server) needs a provider and is
//! explicitly out of scope here; `scripts/eval-monitor-change-ledger.py`
//! wraps this suite and writes the coverage report.
//!
//! Run: `cargo test -p magician --test monitor_golden_scenarios`
//! or `make eval-monitor-golden`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use magician::magician_v2::monitors::{
    monitor_run::{
        advance_source_failures, compare_runs, finalize_run_result, validate_monitor_run_result,
        would_notify, AcceptedMonitorRun, MonitorCursorV1, MonitorFindingClassification,
        MonitorRunResultV1, MonitorRunStatus,
    },
    monitor_spec::{MonitorNotificationPolicy, MonitorSpecV1},
    monitor_updates::{build_monitor_update, update_projects_to_changed},
};

const GOLDEN_TASK_ID: &str = "task_golden_monitor";
const GOLDEN_REVISION: u32 = 3;
const PRINCIPAL: &str = "anonymous";
const WORKSPACE: &str = "default";

/// The plan's required scenario set — a missing or renamed fixture fails
/// the suite loudly instead of silently shrinking coverage.
const REQUIRED_SCENARIOS: [&str; 7] = [
    "public_page_price_change",
    "auth_dashboard_auth_failed",
    "release_notes_new_entry",
    "date_status_flip",
    "transient_failure_then_recovery",
    "unchanged_page",
    "cosmetic_only_change",
];

#[derive(Debug, Deserialize)]
struct GoldenScenario {
    name: String,
    #[allow(dead_code)]
    description: String,
    notification_policy: MonitorNotificationPolicy,
    notify_initial_baseline: bool,
    steps: Vec<GoldenStep>,
}

#[derive(Debug, Deserialize)]
struct GoldenStep {
    label: String,
    execution_id: String,
    incoming: MonitorRunResultV1,
    expect: GoldenExpect,
}

#[derive(Debug, Deserialize)]
struct GoldenExpect {
    status: MonitorRunStatus,
    material: bool,
    would_notify: bool,
    update_recorded: bool,
    projects_to_changed: bool,
    change_fingerprint_present: bool,
    #[serde(default)]
    counts: Option<GoldenCounts>,
    #[serde(default)]
    classifications: Option<BTreeMap<String, MonitorFindingClassification>>,
    /// Expected number of tracked stable keys on the next cursor —
    /// the §7.3 "ledger intact" assertion for degraded scans.
    #[serde(default)]
    retained_ledger_keys: Option<usize>,
    /// Expected consecutive-failure streak per source (§5.5).
    #[serde(default)]
    source_failure_streaks: Option<BTreeMap<String, u32>>,
    /// When true, the next cursor must carry NO failure streaks at all.
    #[serde(default)]
    source_failures_cleared: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GoldenCounts {
    new: u64,
    updated: u64,
    unchanged: u64,
    possibly_removed: u64,
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monitors/golden")
}

fn golden_spec(scenario: &GoldenScenario) -> MonitorSpecV1 {
    let mut spec: MonitorSpecV1 =
        serde_json::from_str(include_str!("fixtures/monitors/monitor_spec_v1.json"))
            .expect("canonical spec fixture decodes");
    spec.notification_policy = scenario.notification_policy.clone();
    spec.notify_initial_baseline = scenario.notify_initial_baseline;
    spec
}

fn load_scenarios() -> Vec<GoldenScenario> {
    let mut scenarios: Vec<GoldenScenario> = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .expect("golden fixture directory exists")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    for path in entries {
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let scenario: GoldenScenario = serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("decoding {}: {error}", path.display()));
        scenarios.push(scenario);
    }
    scenarios
}

/// Advance the hot cursor the way `accept_monitor_run` does for non-failed
/// runs (artifact_v2/service.rs — the comparison arm). Golden steps never
/// use `failed` markers, so the failed arm is not mirrored here.
fn next_cursor(
    previous: Option<&MonitorCursorV1>,
    incoming: &MonitorRunResultV1,
    comparison: &magician::magician_v2::monitors::monitor_run::MonitorRunComparison,
    execution_id: &str,
) -> MonitorCursorV1 {
    MonitorCursorV1 {
        last_accepted_execution_id: execution_id.to_string(),
        last_accepted_change_fingerprint: if comparison.material || comparison.baseline {
            comparison.change_fingerprint.clone()
        } else {
            previous.and_then(|cursor| cursor.last_accepted_change_fingerprint.clone())
        },
        last_complete_scan: comparison.inventory_complete,
        recent_stable_keys: comparison.recent_stable_keys.clone(),
        source_failures: advance_source_failures(
            previous
                .map(|cursor| cursor.source_failures.as_slice())
                .unwrap_or(&[]),
            incoming,
        ),
        updated_at: incoming.completed_at.clone(),
    }
}

#[test]
fn golden_change_ledger_scenarios() {
    let scenarios = load_scenarios();
    let names: Vec<&str> = scenarios
        .iter()
        .map(|scenario| scenario.name.as_str())
        .collect();
    for required in REQUIRED_SCENARIOS {
        assert!(
            names.contains(&required),
            "required golden scenario `{required}` is missing (present: {names:?})"
        );
    }

    for scenario in &scenarios {
        let spec = golden_spec(scenario);
        let mut cursor: Option<MonitorCursorV1> = None;
        assert!(
            !scenario.steps.is_empty(),
            "{}: scenarios need at least one step",
            scenario.name
        );

        for step in &scenario.steps {
            let context = format!("{} / {}", scenario.name, step.label);
            let incoming = &step.incoming;
            assert_eq!(
                incoming.execution_id, step.execution_id,
                "{context}: fixture execution ids must agree"
            );

            // 1) The incoming claim must pass the SAME admission validation
            //    accept_monitor_run applies — golden claims stay honest.
            validate_monitor_run_result(incoming, GOLDEN_TASK_ID, GOLDEN_REVISION).unwrap_or_else(
                |reason| panic!("{context}: incoming claim failed validation: {reason}"),
            );

            // 2) Deterministic comparison + server-owned finalization.
            let comparison = compare_runs(cursor.as_ref(), incoming);
            let finalized =
                finalize_run_result(GOLDEN_TASK_ID, GOLDEN_REVISION, incoming, &comparison);
            let expect = &step.expect;

            assert_eq!(
                finalized.status, expect.status,
                "{context}: finalized status"
            );
            assert_eq!(
                comparison.material, expect.material,
                "{context}: material verdict"
            );
            assert_eq!(
                finalized.change_fingerprint.is_some(),
                expect.change_fingerprint_present,
                "{context}: change fingerprint presence"
            );
            if let Some(counts) = &expect.counts {
                assert_eq!(finalized.counts.new, counts.new, "{context}: counts.new");
                assert_eq!(
                    finalized.counts.updated, counts.updated,
                    "{context}: counts.updated"
                );
                assert_eq!(
                    finalized.counts.unchanged, counts.unchanged,
                    "{context}: counts.unchanged"
                );
                assert_eq!(
                    finalized.counts.possibly_removed, counts.possibly_removed,
                    "{context}: counts.possibly_removed (removal safety)"
                );
            }
            if let Some(classifications) = &expect.classifications {
                for (stable_key, expected) in classifications {
                    let finding = finalized
                        .findings
                        .iter()
                        .find(|finding| finding.stable_key == *stable_key)
                        .unwrap_or_else(|| {
                            panic!("{context}: finding {stable_key} missing from finalized run")
                        });
                    assert_eq!(
                        finding.classification, *expected,
                        "{context}: classification of {stable_key}"
                    );
                }
            }

            // 3) Deterministic notification policy (§7.2 backend-owned).
            let notify = would_notify(&spec, comparison.status, comparison.material);
            assert_eq!(notify, expect.would_notify, "{context}: would_notify");

            // 4) Update record + Today Changed projection (§9.2/§3).
            let advanced = next_cursor(cursor.as_ref(), incoming, &comparison, &step.execution_id);
            let accepted = AcceptedMonitorRun {
                task_id: GOLDEN_TASK_ID.to_string(),
                execution_id: step.execution_id.clone(),
                monitor_revision: GOLDEN_REVISION,
                material: comparison.material,
                would_notify: notify,
                newly_accepted: true,
                source_failures: advanced.source_failures.clone(),
                result: finalized.clone(),
            };
            let update = build_monitor_update(PRINCIPAL, WORKSPACE, &spec, &accepted);
            assert_eq!(
                update.is_some(),
                expect.update_recorded,
                "{context}: update record presence"
            );
            let projects = update
                .as_ref()
                .map(update_projects_to_changed)
                .unwrap_or(false);
            assert_eq!(
                projects, expect.projects_to_changed,
                "{context}: Today Changed projection"
            );
            if let Some(update) = update.as_ref() {
                assert_eq!(
                    update.notification.emitted, notify,
                    "{context}: notification.emitted mirrors would_notify"
                );
            }

            // 5) Hot-state expectations (§7.3 ledger intactness + §5.5
            //    failure streaks).
            if let Some(retained) = expect.retained_ledger_keys {
                assert_eq!(
                    advanced.recent_stable_keys.len(),
                    retained,
                    "{context}: retained ledger keys"
                );
            }
            if let Some(streaks) = &expect.source_failure_streaks {
                for (source, expected_streak) in streaks {
                    let entry = advanced
                        .source_failures
                        .iter()
                        .find(|entry| entry.source == *source)
                        .unwrap_or_else(|| {
                            panic!("{context}: no failure streak tracked for {source}")
                        });
                    assert_eq!(
                        entry.consecutive_failures, *expected_streak,
                        "{context}: failure streak for {source}"
                    );
                }
            }
            if expect.source_failures_cleared == Some(true) {
                assert!(
                    advanced.source_failures.is_empty(),
                    "{context}: failure streaks must be cleared, got {:?}",
                    advanced.source_failures
                );
            }

            cursor = Some(advanced);
        }
    }
}

/// The determinism property the whole eval rests on: replaying a scenario
/// produces byte-identical finalized results and identical update ids.
#[test]
fn golden_scenarios_are_deterministic_across_replays() {
    for scenario in &load_scenarios() {
        let spec = golden_spec(scenario);
        let run_once = || {
            let mut cursor: Option<MonitorCursorV1> = None;
            let mut outcomes: Vec<(MonitorRunResultV1, Option<String>)> = Vec::new();
            for step in &scenario.steps {
                let comparison = compare_runs(cursor.as_ref(), &step.incoming);
                let finalized = finalize_run_result(
                    GOLDEN_TASK_ID,
                    GOLDEN_REVISION,
                    &step.incoming,
                    &comparison,
                );
                let advanced = next_cursor(
                    cursor.as_ref(),
                    &step.incoming,
                    &comparison,
                    &step.execution_id,
                );
                let accepted = AcceptedMonitorRun {
                    task_id: GOLDEN_TASK_ID.to_string(),
                    execution_id: step.execution_id.clone(),
                    monitor_revision: GOLDEN_REVISION,
                    material: comparison.material,
                    would_notify: would_notify(&spec, comparison.status, comparison.material),
                    newly_accepted: true,
                    source_failures: advanced.source_failures.clone(),
                    result: finalized.clone(),
                };
                let update_id = build_monitor_update(PRINCIPAL, WORKSPACE, &spec, &accepted)
                    .map(|update| update.update_id);
                outcomes.push((finalized, update_id));
                cursor = Some(advanced);
            }
            outcomes
        };
        let first = run_once();
        let second = run_once();
        assert_eq!(
            first, second,
            "{}: replay must be byte-identical (deterministic ids/fingerprints)",
            scenario.name
        );
    }
}
