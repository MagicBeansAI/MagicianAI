//! The monitors provider-seam entry (plan workstream 3.2,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! One decision function the run-acceptance handler calls:
//! [`decide_run_acceptance`] sequences everything backend-owned about
//! accepting a monitor run — the failed-run carve-out, the §7 comparison,
//! the next hot-state cursor, the finalized persisted-result shape, and
//! the notify projection — so `ArtifactV2Service::accept_monitor_run` is
//! reduced to task-store integration (write guard, task read, the durable
//! `monitor_run_result` artifact, cursor persistence). The seam OWNS the
//! contract; the store keeps the plumbing.
//!
//! Behavior-identical to the decision block that lived inline in
//! `accept_monitor_run`: the branch bodies moved verbatim, only rebinding
//! `task.state.monitor_cursor` → the `previous_cursor` parameter and
//! `now_rfc3339()` → the `now` parameter (the caller keeps one clock
//! reading for the artifact and the task-state timestamps).

use super::monitor_run::{
    advance_source_failures, compare_runs, finalize_run_result, would_notify, MonitorCursorV1,
    MonitorRunResultV1, MonitorRunStatus,
};
use super::monitor_spec::MonitorSpecV1;

/// The backend-owned decision for one accepted run — everything
/// [`super::provider::decide_run_acceptance`] derives, and nothing the
/// task store contributes (no guards, no artifacts, no persistence).
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorRunAcceptanceDecision {
    /// The finalized result exactly as it must be persisted in the durable
    /// `monitor_run_result` artifact (`finalize_run_result` output; failed
    /// runs have their claimed change fingerprint stripped).
    pub finalized: MonitorRunResultV1,
    /// §7.2 material-change decision (server-owned).
    pub material: bool,
    /// Deterministic policy projection (`would_notify`).
    pub would_notify: bool,
    /// The next hot-state cursor to write on `TaskState.monitor_cursor`.
    pub next_cursor: MonitorCursorV1,
}

/// Decide one run acceptance: pure, deterministic, no I/O.
///
/// Invariants carried over from the inline block this replaced:
/// * a `failed` run never advances the change ledger — the artifact is
///   persisted for history with no change fingerprint, the cursor records
///   the acceptance (idempotency) with `last_complete_scan: false`, and
///   the tracked stable keys / previous change fingerprint / per-source
///   failure streaks survive untouched (a failed run proves nothing
///   absent);
/// * otherwise the §7 comparison finalizes identity, fingerprints, and
///   classifications, the cursor carries the last accepted MATERIAL
///   change fingerprint across quiet runs (§7.4 continuity), and the
///   per-source failure streaks advance from the incoming result (whose
///   source outcomes are copied verbatim into the finalized record).
pub fn decide_run_acceptance(
    spec: &MonitorSpecV1,
    task_id: &str,
    monitor_revision: u32,
    execution_id: &str,
    previous_cursor: Option<&MonitorCursorV1>,
    incoming: MonitorRunResultV1,
    now: String,
) -> MonitorRunAcceptanceDecision {
    let (finalized, material, would_notify, next_cursor) = if incoming.status
        == MonitorRunStatus::Failed
    {
        let mut finalized = incoming;
        finalized.change_fingerprint = None;
        let cursor = MonitorCursorV1 {
            last_accepted_execution_id: execution_id.to_string(),
            last_accepted_change_fingerprint: previous_cursor
                .and_then(|cursor| cursor.last_accepted_change_fingerprint.clone()),
            last_complete_scan: false,
            recent_stable_keys: previous_cursor
                .map(|cursor| cursor.recent_stable_keys.clone())
                .unwrap_or_default(),
            // A failed run proves nothing about real sources — the
            // per-source failure streaks carry forward untouched
            // (mirrors the removal-safety stance above).
            source_failures: previous_cursor
                .map(|cursor| cursor.source_failures.clone())
                .unwrap_or_default(),
            updated_at: now,
        };
        let would_notify = would_notify(spec, MonitorRunStatus::Failed, false);
        (finalized, false, would_notify, cursor)
    } else {
        let comparison = compare_runs(previous_cursor, &incoming);
        let cursor = MonitorCursorV1 {
            last_accepted_execution_id: execution_id.to_string(),
            // The fingerprint of the last accepted MATERIAL change —
            // carried forward across quiet runs for §7.4 continuity.
            last_accepted_change_fingerprint: if comparison.material || comparison.baseline {
                comparison.change_fingerprint.clone()
            } else {
                previous_cursor.and_then(|cursor| cursor.last_accepted_change_fingerprint.clone())
            },
            last_complete_scan: comparison.inventory_complete,
            recent_stable_keys: comparison.recent_stable_keys.clone(),
            // §5.5: advance per-source CONSECUTIVE failure streaks —
            // ok+complete resets, failing increments, unscanned carries.
            // The source outcomes are copied verbatim into the finalized
            // record, so advancing from the incoming result is
            // equivalent.
            source_failures: advance_source_failures(
                previous_cursor
                    .map(|cursor| cursor.source_failures.as_slice())
                    .unwrap_or(&[]),
                &incoming,
            ),
            updated_at: now,
        };
        let would_notify = would_notify(spec, comparison.status, comparison.material);
        // Single shared construction (`finalize_run_result`) so the
        // persisted record and the by-construction validation tests can
        // never drift: complete_scan persists as the inventory-complete
        // verdict, and baselines never carry a change fingerprint on the
        // wire (the cursor keeps it).
        let finalized = finalize_run_result(task_id, monitor_revision, &incoming, &comparison);
        (finalized, comparison.material, would_notify, cursor)
    };
    MonitorRunAcceptanceDecision {
        finalized,
        material,
        would_notify,
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::super::monitor_run::{
        compare_runs, finalize_run_result, would_notify, MonitorCountsV1,
        MonitorFindingClassification, MonitorFindingV1, MonitorSourceOutcomeStatus,
        MonitorSourceOutcomeV1,
    };
    use super::super::monitor_spec::{
        validate_and_normalize, MonitorMatchMode, MonitorNotificationPolicy, MonitorSources,
        MonitorSpecV1, MONITOR_SPEC_SCHEMA_VERSION,
    };
    use super::*;

    /// The CANONICAL Phase 0 wire fixture — the same bytes the phase0 wire
    /// oracles and the web/iOS contract checks pin.
    const SPEC_FIXTURE: &str =
        include_str!("../../../tests/fixtures/monitors/monitor_spec_v1.json");

    fn fixture_spec() -> MonitorSpecV1 {
        serde_json::from_str(SPEC_FIXTURE).expect("spec fixture decodes")
    }

    fn spec_with(policy: MonitorNotificationPolicy, baseline: bool) -> MonitorSpecV1 {
        MonitorSpecV1 {
            schema_version: MONITOR_SPEC_SCHEMA_VERSION,
            objective: "o".to_string(),
            query_seeds: Vec::new(),
            sources: MonitorSources {
                urls: vec!["https://example.com".to_string()],
                domains: Vec::new(),
                authenticated_sources: Vec::new(),
            },
            include_rules: Vec::new(),
            exclude_rules: Vec::new(),
            match_mode: MonitorMatchMode::Balanced,
            notification_policy: policy,
            notify_initial_baseline: baseline,
        }
    }

    /// A minimal valid finding; only the fact-bearing fields matter here.
    fn finding() -> MonitorFindingV1 {
        MonitorFindingV1 {
            stable_key: "url:example.com/pricing".to_string(),
            title: "Pricing".to_string(),
            canonical_url: Some("https://example.com/pricing".to_string()),
            source: "https://example.com".to_string(),
            observed_at: "2026-07-23T06:00:30Z".to_string(),
            published_at: None,
            summary: "s".to_string(),
            why_it_matters: "w".to_string(),
            entities: Vec::new(),
            evidence: Vec::new(),
            content_fingerprint: String::new(),
            classification: MonitorFindingClassification::New,
        }
    }

    fn run_result(status: MonitorRunStatus, complete_scan: bool) -> MonitorRunResultV1 {
        MonitorRunResultV1 {
            monitor_task_id: "task_monitor_1".to_string(),
            execution_id: "exec_1".to_string(),
            monitor_revision: 2,
            started_at: "2026-07-23T06:00:00Z".to_string(),
            completed_at: "2026-07-23T06:01:00Z".to_string(),
            status,
            complete_scan,
            source_outcomes: vec![MonitorSourceOutcomeV1 {
                source: "https://example.com".to_string(),
                status: MonitorSourceOutcomeStatus::Ok,
                complete: complete_scan,
                items_scanned: 1,
                note: None,
            }],
            counts: MonitorCountsV1 {
                scanned: 1,
                new: 1,
                updated: 0,
                unchanged: 0,
                possibly_removed: 0,
            },
            findings: vec![finding()],
            run_fingerprint: "rf_1111111111111111".to_string(),
            change_fingerprint: None,
            access_problem: None,
        }
    }

    // ── Spec admission through the seam fns ────────────────────────────

    #[test]
    fn spec_round_trips_through_the_seam_admission_gate() {
        let mut spec = fixture_spec();
        let pristine = spec.clone();
        validate_and_normalize(&mut spec).expect("canonical fixture validates via the seam");
        assert_eq!(spec, pristine, "fixture is already normalized");

        // The seam's wire surface is the spec type itself: serializing the
        // normalized spec reproduces the fixture byte-for-byte.
        let wire = serde_json::to_value(&spec).expect("serializes");
        let original: serde_json::Value =
            serde_json::from_str(SPEC_FIXTURE).expect("fixture is JSON");
        assert_eq!(wire, original);

        // A messy spec normalizes to the identical contract (idempotent
        // admission), matching the fixture-derived normalized form.
        let mut messy = fixture_spec();
        messy.objective = format!("  {}  ", messy.objective);
        let duplicate_seed = format!("  {}  ", messy.query_seeds[0]);
        messy.query_seeds.insert(0, duplicate_seed);
        validate_and_normalize(&mut messy).expect("messy spec normalizes");
        assert_eq!(messy, spec, "normalization collapses cosmetics only");
    }

    // ── would_notify truth-table parity with the phase0 wire oracle ────

    #[test]
    fn would_notify_truth_table_matches_the_pinned_oracle_semantics() {
        // material_changes: material change ⇒ notify; quiet run ⇒ not;
        // quiet baseline ⇒ not; opted-in baseline ⇒ notify.
        let quiet_baseline = spec_with(MonitorNotificationPolicy::MaterialChanges, false);
        assert!(would_notify(
            &quiet_baseline,
            MonitorRunStatus::Changed,
            true
        ));
        assert!(!would_notify(
            &quiet_baseline,
            MonitorRunStatus::Changed,
            false
        ));
        assert!(!would_notify(
            &quiet_baseline,
            MonitorRunStatus::Baseline,
            false
        ));

        let loud_baseline = spec_with(MonitorNotificationPolicy::MaterialChanges, true);
        assert!(would_notify(
            &loud_baseline,
            MonitorRunStatus::Baseline,
            false
        ));

        // every_run always; never — never (even a material change).
        assert!(would_notify(
            &spec_with(MonitorNotificationPolicy::EveryRun, false),
            MonitorRunStatus::Unchanged,
            false
        ));
        assert!(!would_notify(
            &spec_with(MonitorNotificationPolicy::Never, true),
            MonitorRunStatus::Changed,
            true
        ));
    }

    // ── decide_run_acceptance parity with the composed seam primitives ─

    #[test]
    fn acceptance_decision_composes_comparison_finalization_and_policy() {
        let spec = fixture_spec();
        let incoming = run_result(MonitorRunStatus::Changed, true);

        // Baseline (no previous cursor): the first accepted run is a
        // non-material baseline whose finalized record carries NO change
        // fingerprint while the cursor keeps one for §7.4 continuity.
        let baseline = decide_run_acceptance(
            &spec,
            "task_monitor_1",
            2,
            "exec_1",
            None,
            incoming.clone(),
            "2026-07-23T06:01:00Z".to_string(),
        );
        assert!(baseline
            .next_cursor
            .last_accepted_change_fingerprint
            .is_some());
        assert!(baseline.finalized.change_fingerprint.is_none());
        assert!(!baseline.material);
        assert!(
            !baseline.would_notify,
            "quiet baseline under material_changes"
        );

        // The decision is exactly the composition of the seam primitives
        // the handler used to sequence inline.
        let comparison = compare_runs(None, &incoming);
        let finalized = finalize_run_result("task_monitor_1", 2, &incoming, &comparison);
        assert_eq!(baseline.finalized, finalized);
        assert_eq!(baseline.material, comparison.material);
        assert_eq!(
            baseline.would_notify,
            would_notify(&spec, comparison.status, comparison.material)
        );

        // Second run over the baseline cursor with the SAME facts is
        // unchanged: quiet, no change fingerprint, §7.4 continuity
        // carries the baseline's fingerprint forward.
        let second = decide_run_acceptance(
            &spec,
            "task_monitor_1",
            2,
            "exec_2",
            Some(&baseline.next_cursor),
            incoming.clone(),
            "2026-07-23T07:01:00Z".to_string(),
        );
        assert_eq!(second.finalized.status, MonitorRunStatus::Unchanged);
        assert!(!second.material);
        assert!(!second.would_notify);
        assert_eq!(
            second.next_cursor.last_accepted_change_fingerprint,
            baseline.next_cursor.last_accepted_change_fingerprint,
            "quiet runs carry the last accepted MATERIAL change forward"
        );

        // A changed fact on the same stable key is material and notifies.
        let mut changed = incoming;
        changed.findings[0].title = "Pricing — updated".to_string();
        changed.execution_id = "exec_3".to_string();
        let third = decide_run_acceptance(
            &spec,
            "task_monitor_1",
            2,
            "exec_3",
            Some(&second.next_cursor),
            changed,
            "2026-07-23T08:01:00Z".to_string(),
        );
        assert_eq!(third.finalized.status, MonitorRunStatus::Changed);
        assert!(third.material);
        assert!(third.would_notify);
        assert_eq!(
            third.finalized.change_fingerprint,
            third.next_cursor.last_accepted_change_fingerprint
        );
    }

    #[test]
    fn failed_run_acceptance_never_advances_the_change_ledger() {
        let spec = fixture_spec();
        let mut incoming = run_result(MonitorRunStatus::Changed, true);
        incoming.status = MonitorRunStatus::Failed;
        incoming.change_fingerprint = Some("chg_deadbeefdeadbeef".to_string());

        let previous = decide_run_acceptance(
            &spec,
            "task_monitor_1",
            2,
            "exec_1",
            None,
            run_result(MonitorRunStatus::Changed, true),
            "2026-07-23T06:01:00Z".to_string(),
        );

        let decision = decide_run_acceptance(
            &spec,
            "task_monitor_1",
            2,
            "exec_2",
            Some(&previous.next_cursor),
            incoming,
            "2026-07-23T07:01:00Z".to_string(),
        );
        // The claimed fingerprint is stripped; nothing material is claimed.
        assert_eq!(decision.finalized.status, MonitorRunStatus::Failed);
        assert!(decision.finalized.change_fingerprint.is_none());
        assert!(!decision.material);
        assert!(!decision.would_notify);
        // The cursor records the acceptance for idempotency but advances
        // nothing: keys, the material-change fingerprint, and the
        // inventory verdict survive untouched (last_complete_scan false).
        assert_eq!(
            decision.next_cursor.recent_stable_keys,
            previous.next_cursor.recent_stable_keys
        );
        assert_eq!(
            decision.next_cursor.last_accepted_change_fingerprint,
            previous.next_cursor.last_accepted_change_fingerprint
        );
        assert!(!decision.next_cursor.last_complete_scan);
        assert_eq!(decision.next_cursor.last_accepted_execution_id, "exec_2");
    }
}
