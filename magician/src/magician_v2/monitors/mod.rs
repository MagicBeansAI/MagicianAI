//! The monitors provider seam (plan workstream 3.2,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! A monitor is an ordinary persistent scheduled task with a typed monitor
//! contract attached: there is no second scheduler, queue, store, or
//! runtime (`docs/features/monitors.md`). This module owns the PRODUCT
//! side of that sentence — the typed contract, the deterministic diff and
//! notification policy, and the update/feedback ledger semantics — as a
//! seam-registered module (default form; not an app package: no stated
//! packaging benefit, and packaging would imply a monitors-UI rewrite).
//!
//! What moved here in 3.2, behavior-identical (pure move + seam):
//! - `monitor_spec` — the `MonitorSpecV1` admission gate
//!   (`validate_and_normalize`) and the preview→create contract
//!   fingerprint.
//! - `monitor_run` — the `MonitorRunResultV1` contract, stable identity,
//!   fingerprints, `compare_runs` material-change/removal-safety
//!   semantics, the bounded cursor, and the `would_notify` policy.
//! - `monitor_updates` — notification-policy enforcement, the durable
//!   `MonitorUpdateDetailV1` record, §7.4 dedupe keys, and the
//!   access-problem Attention feed item.
//! - `monitor_feedback` — the useful / not-relevant feedback records and
//!   their latest-wins fold.
//! - `provider` — the seam entry the run-acceptance handler calls: one
//!   decision function that sequences validation, comparison, cursor
//!   construction, and the notify projection for an accepted run.
//!
//! Compat shims removed (phase 5, batch 3): the former
//! `artifact_v2::monitor_{spec,run,updates,feedback}` re-export modules are
//! gone; every import (service.rs, task_api_v3, monitors_api, the chat
//! tools, feed projections, and the phase0 wire oracles) now points here.
//! The wire shapes are pinned by `magician/tests/phase0_wire_oracles.rs`
//! and the canonical fixtures — the types did not change shape.
//!
//! What did not move (Layer 1 stays Layer 1):
//! - Scheduled task creation remains the ordinary V3 task path —
//!   `create_task` + `update_task` in `artifact_v2/service.rs` (the shared
//!   composition lives in `magician_v2::monitor_support::create_monitor_task`).
//! - Task-store integration stays in `artifact_v2/service.rs`: the write
//!   guards, persisted `monitor_run_result` artifacts, the per-task
//!   `monitor_updates.jsonl` / `monitor_feedback.jsonl` ledgers with
//!   bounded retention, the attention-funnel dedupe rows, and the §12
//!   trace events. The service calls THIS module's decision functions; it
//!   never re-derives product policy.
//! - Content acquisition continues through the existing
//!   `content_read`/`content_search` ladder (`content_sources`); nothing
//!   here fetches, and no second fetch path was created.
//! - `magician_v2::monitor_support` stays put: its schedule helpers are
//!   shared with non-monitor callers (`storage/list_index`), so folding it
//!   into this seam would invert the layer dependency.

pub mod monitor_feedback;
pub mod monitor_run;
pub mod monitor_spec;
pub mod monitor_updates;
pub mod provider;

pub use monitor_feedback::{
    build_monitor_feedback, latest_feedback_per_update, monitor_feedback_id,
    normalize_feedback_note, MonitorFeedbackEvidenceV1, MonitorFeedbackOutcome,
    MonitorFeedbackVerdict, MonitorUpdateFeedbackV1, MONITOR_FEEDBACK_NOTE_MAX_CHARS,
};
pub use monitor_run::{
    advance_source_failures, compare_runs, finalize_run_result, run_fingerprint, stable_key,
    validate_monitor_run_result, would_notify, AcceptedMonitorRun, MonitorAccessProblemV1,
    MonitorCountsV1, MonitorCursorV1, MonitorEvidenceV1, MonitorFindingClassification,
    MonitorFindingV1, MonitorRunComparison, MonitorRunResultV1, MonitorRunStatus,
    MonitorSourceFailureEntry, MonitorSourceOutcomeStatus, MonitorSourceOutcomeV1,
    MonitorStableKeyEntry, StableKeyInputs, MONITOR_RECENT_STABLE_KEYS_CAP,
    MONITOR_REMOVAL_MISS_THRESHOLD, MONITOR_RUN_MAX_EVIDENCE_PER_FINDING, MONITOR_RUN_MAX_FINDINGS,
    MONITOR_SOURCE_FAILURES_CAP, MONITOR_SOURCE_FAILURE_ATTENTION_THRESHOLD,
};
pub use monitor_spec::{
    monitor_contract_fingerprint, validate_and_normalize, MonitorMatchMode,
    MonitorNotificationPolicy, MonitorSources, MonitorSpecV1, MONITOR_SPEC_SCHEMA_VERSION,
};
pub use monitor_updates::{
    build_monitor_update, monitor_access_problem_feed_item, monitor_access_problem_item_id,
    notification_dedupe_key, notification_suppression_reason, update_projects_to_changed,
    MonitorNotificationSuppressionReason, MonitorUpdateDetailV1, MonitorUpdateNotificationV1,
    MONITOR_LEDGER_RETENTION_CAP, MONITOR_UPDATE_CHANNEL_TODAY_CHANGED,
};
pub use provider::{decide_run_acceptance, MonitorRunAcceptanceDecision};
