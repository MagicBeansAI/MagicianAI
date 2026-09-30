//! Recurring Monitors (Phase 3) — deterministic notification-policy
//! enforcement and the durable monitor-update record.
//!
//! Plan: `docs/plans/2026-07-21-recurring-monitors-productization-design-implementation.md`
//! §7.2 (backend-owned gating — "Notification gating must not be a prompt
//! convention"), §7.4 (dedupe key), §9.2 (surface rules), §3 (semantic
//! boundaries: unchanged runs never reach Today/Attention/notifications
//! under the default policy).
//!
//! The wire shape of [`MonitorUpdateDetailV1`] is pinned by the canonical
//! Phase 0 fixture `magician/tests/fixtures/monitors/monitor_update_detail_v1.json`
//! — web and iOS read the same file, so the typed decode test here breaks
//! together with the structural checks in `tests/monitor_contract_fixtures.rs`.
//!
//! Everything in this module is pure and deterministic — no LLM, no I/O, no
//! clock. `ArtifactV2Service::project_monitor_run_outcome` owns persistence
//! (the per-task `monitor_updates.jsonl` ledger + the attention-funnel
//! dedupe row) and calls into these builders.

use serde::{Deserialize, Serialize};

use super::monitor_run::{
    AcceptedMonitorRun, MonitorAccessProblemV1, MonitorFindingV1, MonitorRunResultV1,
    MonitorRunStatus, MonitorSourceFailureEntry,
};
use super::monitor_spec::{MonitorNotificationPolicy, MonitorSpecV1};
use crate::magician_v2::feed::{FeedAction, FeedItem, FeedItemStatus, FeedItemType};

/// The single v1 notification channel: material updates surface as Today
/// `Changed` cards (plan §9.2 rule 1). OS pushes ride this same projection
/// (Tauri/iOS render what the backend already deduped), so the §7.4 dedupe
/// key is enforced here, not in any push layer.
pub const MONITOR_UPDATE_CHANNEL_TODAY_CHANGED: &str = "today_changed";

/// Phase 6 bounded retention (plan §13 Phase 6): the per-task
/// `monitor_updates.jsonl` and `monitor_feedback.jsonl` ledgers are capped
/// at this many records — on append past the cap the ledger is rewritten
/// atomically keeping the NEWEST records only.
///
/// The dedupe property that matters survives compaction because both
/// `update_id` and `feedback_id` are deterministic hashes: a replayed
/// acceptance of a SURVIVING record re-derives the same id and is still
/// skipped. A `change_fingerprint` OLDER than the retention horizon CAN
/// re-emit after compaction (its update record is gone, so the ledger
/// dedupe no longer sees the id) — accepted deliberately: a change that
/// last surfaced 500 updates ago resurfacing again is arguably correct
/// behavior, and the durable per-channel funnel row (`monitor-notify:*`,
/// SQLite — not subject to this compaction) still records the §7.4 replay
/// as `monitor_notification_suppressed(dedupe_replay)` observability.
pub const MONITOR_LEDGER_RETENTION_CAP: usize = 500;

/// Bounded reason enum for `monitor_notification_suppressed` (plan §12
/// rule 4). Wire tokens are the snake_case serde names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorNotificationSuppressionReason {
    /// The monitor's notification policy is `never` — history is recorded,
    /// nothing may notify.
    PolicyNever,
    /// Nothing material to notify under `material_changes`: quiet
    /// unchanged/degraded/failed runs and non-opted-in baselines. (Degraded
    /// runs additionally raise `monitor_source_access_failed` — the access
    /// problem is its own lane, never a change notification.)
    Unchanged,
    /// The §7.4 durable per-channel dedupe row already existed for this
    /// dedupe key when a NEWLY recorded update tried to notify — the same
    /// change fingerprint resurfaced (e.g. past the compaction horizon) and
    /// the durable dedupe suppressed the duplicate notification.
    DedupeReplay,
    /// An `every_run` receipt on a quiet (non-changed, non-baseline) run:
    /// recorded and emitted into the Updates history per the user's opt-in,
    /// but never surfaced as a Today `Changed` notification (§3).
    QuietEveryRun,
}

impl MonitorNotificationSuppressionReason {
    /// Stable wire token for event metadata.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PolicyNever => "policy_never",
            Self::Unchanged => "unchanged",
            Self::DedupeReplay => "dedupe_replay",
            Self::QuietEveryRun => "quiet_every_run",
        }
    }
}

/// Deterministic §12 suppression classification for one accepted run's
/// projection outcome. `None` means the update notifies on the Changed
/// channel (the `dedupe_replay` case is decided later, at the durable
/// dedupe-row insert — see `record_monitor_notification_outcome`).
pub fn notification_suppression_reason(
    policy: &MonitorNotificationPolicy,
    update: Option<&MonitorUpdateDetailV1>,
) -> Option<MonitorNotificationSuppressionReason> {
    match update {
        Some(update) if update.notification.emitted => {
            if update_projects_to_changed(update) {
                None
            } else {
                // Emitted per every_run opt-in but quiet — Updates history
                // only, never a Changed card (§3).
                Some(MonitorNotificationSuppressionReason::QuietEveryRun)
            }
        },
        // Recorded for history but policy-suppressed (material under
        // `never`, or a quiet baseline without the opt-in).
        Some(_) => Some(match policy {
            MonitorNotificationPolicy::Never => MonitorNotificationSuppressionReason::PolicyNever,
            _ => MonitorNotificationSuppressionReason::Unchanged,
        }),
        // No update record at all: quiet unchanged/degraded/failed runs
        // under material_changes (or never).
        None => Some(match policy {
            MonitorNotificationPolicy::Never => MonitorNotificationSuppressionReason::PolicyNever,
            _ => MonitorNotificationSuppressionReason::Unchanged,
        }),
    }
}

/// Bounds mirrored from the Phase 1/2 admission style.
const MAX_HEADLINE_CHARS: usize = 160;
const MAX_SUMMARY_CHARS: usize = 500;

/// Notification block on an update record (fixture shape). `emitted` records
/// the backend's policy decision for this update: `false` means the record
/// exists for history (e.g. a quiet baseline, or a material change under the
/// `never` policy) but no surface may show it as a notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorUpdateNotificationV1 {
    pub policy: MonitorNotificationPolicy,
    pub emitted: bool,
    pub channel: String,
    /// §7.4: EXACTLY `scope:task_id:revision:change_fingerprint:channel`
    /// where scope is `principal/workspace`. For updates without a change
    /// fingerprint (every-run receipts on quiet runs, and ALL baselines —
    /// finalized baseline results never carry one) the fingerprint slot
    /// carries the execution id, so each run keys its own notification
    /// while retries/restarts of the SAME run still collide.
    pub dedupe_key: String,
}

/// One durable monitor update (fixture:
/// `tests/fixtures/monitors/monitor_update_detail_v1.json`). Persisted as one
/// line of the per-task `monitor_updates.jsonl` ledger and served verbatim by
/// `GET /monitors/{task_id}/updates` and `GET /monitor-updates`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorUpdateDetailV1 {
    /// `mu_<16-hex blake3 of the dedupe key>` — deterministic, so a replayed
    /// acceptance rebuilds the SAME id and the ledger append is idempotent.
    pub update_id: String,
    pub monitor_task_id: String,
    pub monitor_revision: u32,
    pub execution_id: String,
    /// The producing run's `completed_at` (deterministic — never "now").
    pub occurred_at: String,
    pub status: MonitorRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_fingerprint: Option<String>,
    pub headline: String,
    pub summary: String,
    /// The producing run's MATERIAL findings (the fixture's cross-check:
    /// update findings == the changed run's non-unchanged findings).
    pub findings: Vec<MonitorFindingV1>,
    pub notification: MonitorUpdateNotificationV1,
}

/// §7.4 notification dedupe key: `(scope, monitor_task_id, monitor_revision,
/// change_fingerprint, channel)` rendered exactly as the canonical fixture
/// shows: `principal/workspace:task_id:revision:fingerprint:channel`.
pub fn notification_dedupe_key(
    principal: &str,
    workspace: &str,
    task_id: &str,
    monitor_revision: u32,
    fingerprint_component: &str,
    channel: &str,
) -> String {
    format!(
        "{principal}/{workspace}:{task_id}:{monitor_revision}:{fingerprint_component}:{channel}"
    )
}

/// The fingerprint slot of the dedupe key: the run's change fingerprint when
/// one exists; otherwise the execution id (see
/// [`MonitorUpdateNotificationV1::dedupe_key`]).
pub fn update_fingerprint_component(result: &MonitorRunResultV1) -> String {
    result
        .change_fingerprint
        .clone()
        .unwrap_or_else(|| result.execution_id.clone())
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut output: String = trimmed.chars().take(max_chars.saturating_sub(3)).collect();
    output.push_str("...");
    output
}

fn material_findings(result: &MonitorRunResultV1) -> Vec<MonitorFindingV1> {
    result
        .findings
        .iter()
        .filter(|finding| finding.classification.is_material())
        .cloned()
        .collect()
}

fn update_headline(result: &MonitorRunResultV1, material: &[MonitorFindingV1]) -> String {
    match result.status {
        MonitorRunStatus::Changed => match material {
            [only] => truncate_chars(&only.title, MAX_HEADLINE_CHARS),
            [first, ..] => truncate_chars(
                &format!("{} changes: {}", material.len(), first.title),
                MAX_HEADLINE_CHARS,
            ),
            [] => "Material change detected".to_string(),
        },
        MonitorRunStatus::Baseline => format!(
            "Baseline captured: now tracking {} item{}",
            result.findings.len(),
            if result.findings.len() == 1 { "" } else { "s" },
        ),
        MonitorRunStatus::Unchanged => "No changes detected".to_string(),
        MonitorRunStatus::Degraded => match &result.access_problem {
            Some(problem) => truncate_chars(
                &format!(
                    "Scan degraded: {} ({})",
                    problem.source,
                    problem.kind.as_wire_str()
                ),
                MAX_HEADLINE_CHARS,
            ),
            None => "Scan degraded".to_string(),
        },
        MonitorRunStatus::Failed => "Monitor run failed".to_string(),
    }
}

fn update_summary(result: &MonitorRunResultV1, material: &[MonitorFindingV1]) -> String {
    match result.status {
        MonitorRunStatus::Changed => {
            let joined = material
                .iter()
                .map(|finding| finding.summary.trim())
                .filter(|summary| !summary.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                format!(
                    "{} new, {} updated, {} possibly removed.",
                    result.counts.new, result.counts.updated, result.counts.possibly_removed
                )
            } else {
                truncate_chars(&joined, MAX_SUMMARY_CHARS)
            }
        },
        MonitorRunStatus::Baseline => format!(
            "First scan recorded {} item{} as the baseline; future runs report material changes only.",
            result.counts.scanned,
            if result.counts.scanned == 1 { "" } else { "s" },
        ),
        MonitorRunStatus::Unchanged => format!(
            "Scan completed with no material change ({} item{} unchanged).",
            result.counts.unchanged,
            if result.counts.unchanged == 1 { "" } else { "s" },
        ),
        MonitorRunStatus::Degraded => match &result.access_problem {
            Some(problem) => truncate_chars(&problem.message, MAX_SUMMARY_CHARS),
            None => "One or more sources could not be fully scanned; nothing was treated as removed.".to_string(),
        },
        MonitorRunStatus::Failed => "The run did not produce a valid monitor result.".to_string(),
    }
}

/// Wire token for a source-outcome status without relying on serde internals.
trait AsWireStr {
    fn as_wire_str(&self) -> &'static str;
}

impl AsWireStr for super::monitor_run::MonitorSourceOutcomeStatus {
    fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::AuthFailed => "auth_failed",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::Error => "error",
        }
    }
}

/// Whether an accepted run mints an update record at all.
///
/// * material runs — always (even under `never`, so the Updates tab keeps
///   the material-change history; `notification.emitted` records the policy)
/// * baselines — always (the "Latest" tab shows baseline-or-latest-update)
/// * `every_run` policy — every accepted run, as an explicit user opt-in
///   receipt (§6.1)
/// * everything else (quiet unchanged/degraded/failed runs under the default
///   policy) — no record; the Runs tab already holds the full run history.
fn update_worthy(spec: &MonitorSpecV1, accepted: &AcceptedMonitorRun) -> bool {
    accepted.material
        || accepted.result.status == MonitorRunStatus::Baseline
        || spec.notification_policy == MonitorNotificationPolicy::EveryRun
}

/// Build the deterministic update record for one accepted run, or `None`
/// when the run is not update-worthy. Pure — the caller owns persistence and
/// dedupe.
pub fn build_monitor_update(
    principal: &str,
    workspace: &str,
    spec: &MonitorSpecV1,
    accepted: &AcceptedMonitorRun,
) -> Option<MonitorUpdateDetailV1> {
    if !update_worthy(spec, accepted) {
        return None;
    }
    let result = &accepted.result;
    let fingerprint_component = update_fingerprint_component(result);
    let dedupe_key = notification_dedupe_key(
        principal,
        workspace,
        &accepted.task_id,
        accepted.monitor_revision,
        &fingerprint_component,
        MONITOR_UPDATE_CHANNEL_TODAY_CHANGED,
    );
    let update_id = format!("mu_{}", &blake3::hash(dedupe_key.as_bytes()).to_hex()[..16]);
    let material = material_findings(result);
    let headline = update_headline(result, &material);
    let summary = update_summary(result, &material);
    // Baselines list their (new) findings so the first card can show what is
    // now tracked; material runs list exactly their material findings.
    let findings = if result.status == MonitorRunStatus::Baseline {
        result.findings.clone()
    } else {
        material
    };
    Some(MonitorUpdateDetailV1 {
        update_id,
        monitor_task_id: accepted.task_id.clone(),
        monitor_revision: accepted.monitor_revision,
        execution_id: accepted.execution_id.clone(),
        occurred_at: result.completed_at.clone(),
        status: result.status,
        change_fingerprint: result.change_fingerprint.clone(),
        headline,
        summary,
        findings,
        notification: MonitorUpdateNotificationV1 {
            policy: spec.notification_policy.clone(),
            // Backend-owned policy decision (§7.2). `would_notify` already
            // encodes material_changes/every_run/never + the baseline
            // opt-in.
            emitted: accepted.would_notify,
            channel: MONITOR_UPDATE_CHANNEL_TODAY_CHANGED.to_string(),
            dedupe_key,
        },
    })
}

/// §9.2/§3 Today rule: only EMITTED changed runs (and emitted opted-in
/// baselines) become Today `Changed` cards. Unchanged and degraded runs never
/// do — even under `every_run`, whose quiet-run receipts stay in the Updates
/// history without demanding attention.
pub fn update_projects_to_changed(update: &MonitorUpdateDetailV1) -> bool {
    update.notification.emitted
        && matches!(
            update.status,
            MonitorRunStatus::Changed | MonitorRunStatus::Baseline
        )
}

// ─── Access-problem Attention projection (§5.5) ──────────────────────────

/// Deterministic feed-item id for one monitor+source access problem, so the
/// escalation upserts idempotently and auto-resolve can remove it by id.
pub fn monitor_access_problem_item_id(task_id: &str, source: &str) -> String {
    let digest = blake3::hash(source.trim().as_bytes()).to_hex();
    format!("monitor_access_problem:{task_id}:{}", &digest[..16])
}

/// Build the Needs You escalation item for a repeatedly failing source.
///
/// `item_type: Escalation` + `status: NeedsAction` is the existing feed
/// contract that lands in BOTH the attention `escalations` lane
/// (`attention_lane_from_parts`) and Today `Needs You` — no new lane store.
/// Metadata carries the monitor task id, source, and run/update ids for
/// exact deep-linking (`magican://task/{id}` rides the task_id field).
#[allow(clippy::too_many_arguments)]
pub fn monitor_access_problem_feed_item(
    principal: &str,
    workspace: &str,
    task_id: &str,
    task_title: &str,
    problem: &MonitorAccessProblemV1,
    entry: &MonitorSourceFailureEntry,
    execution_id: &str,
    update_id: Option<&str>,
    now_ms: i64,
) -> FeedItem {
    let since_ms = chrono::DateTime::parse_from_rfc3339(&entry.since)
        .map(|value| value.timestamp_millis())
        .unwrap_or(now_ms);
    // Canonical monitor route (web `monitorsTaskRoute`) — the plain
    // /tasks?task= view is not what the web reads for monitors.
    let deep_link = crate::magician_v2::feed::action_adapter::monitor_deep_link(task_id, update_id);
    FeedItem {
        id: monitor_access_problem_item_id(task_id, &problem.source),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        item_type: FeedItemType::Escalation,
        task_id: Some(task_id.to_string()),
        ui_thread_id: None,
        agent_id: None,
        title: truncate_chars(
            &format!("Monitor needs access: {}", problem.source),
            MAX_HEADLINE_CHARS,
        ),
        summary: Some(truncate_chars(
            &format!(
                "{} — failing for {} consecutive run{} on \"{}\".",
                problem.message,
                entry.consecutive_failures,
                if entry.consecutive_failures == 1 {
                    ""
                } else {
                    "s"
                },
                truncate_chars(task_title, 80),
            ),
            MAX_SUMMARY_CHARS,
        )),
        status: FeedItemStatus::NeedsAction,
        created_at: since_ms,
        updated_at: now_ms,
        actions: vec![FeedAction {
            id: "open_task".to_string(),
            label: "Open monitor".to_string(),
            action_type: Some("open_task".to_string()),
            payload: serde_json::json!({
                "url": deep_link,
                "task_id": task_id,
            }),
        }],
        metadata: serde_json::json!({
            "attention_kind": "monitor_access_problem",
            "monitor_task_id": task_id,
            "source": problem.source,
            "access_kind": problem.kind.as_wire_str(),
            "message": problem.message,
            "since": entry.since,
            "consecutive_failures": entry.consecutive_failures,
            "execution_id": execution_id,
            "update_id": update_id,
            "deep_link": deep_link,
        }),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::Value;

    use super::super::monitor_run::{
        would_notify, MonitorCountsV1, MonitorSourceOutcomeStatus, MonitorSourceOutcomeV1,
    };
    use super::*;

    /// The CANONICAL Phase 0 wire fixtures — shared byte-for-byte with
    /// `tests/monitor_contract_fixtures.rs` and the web/iOS contract checks.
    const UPDATE_FIXTURE: &str =
        include_str!("../../../tests/fixtures/monitors/monitor_update_detail_v1.json");
    const CHANGED_FIXTURE: &str =
        include_str!("../../../tests/fixtures/monitors/monitor_run_result_v1_changed.json");
    const SPEC_FIXTURE: &str =
        include_str!("../../../tests/fixtures/monitors/monitor_spec_v1.json");

    fn fixture_spec() -> MonitorSpecV1 {
        serde_json::from_str(SPEC_FIXTURE).expect("spec fixture decodes")
    }

    fn changed_result() -> MonitorRunResultV1 {
        serde_json::from_str(CHANGED_FIXTURE).expect("changed fixture decodes")
    }

    fn accepted_from(result: MonitorRunResultV1, spec: &MonitorSpecV1) -> AcceptedMonitorRun {
        let material = result.status == MonitorRunStatus::Changed;
        AcceptedMonitorRun {
            task_id: result.monitor_task_id.clone(),
            execution_id: result.execution_id.clone(),
            monitor_revision: result.monitor_revision,
            material,
            would_notify: would_notify(spec, result.status, material),
            newly_accepted: true,
            source_failures: Vec::new(),
            result,
        }
    }

    fn unchanged_result() -> MonitorRunResultV1 {
        MonitorRunResultV1 {
            monitor_task_id: "task_monitor_fixture_001".to_string(),
            execution_id: "exec_quiet_1".to_string(),
            monitor_revision: 2,
            started_at: "2026-07-23T06:00:00Z".to_string(),
            completed_at: "2026-07-23T06:01:00Z".to_string(),
            status: MonitorRunStatus::Unchanged,
            complete_scan: true,
            source_outcomes: vec![MonitorSourceOutcomeV1 {
                source: "https://acme-robotics.example/pricing".to_string(),
                status: MonitorSourceOutcomeStatus::Ok,
                complete: true,
                items_scanned: 12,
                note: None,
            }],
            counts: MonitorCountsV1 {
                scanned: 12,
                new: 0,
                updated: 0,
                unchanged: 12,
                possibly_removed: 0,
            },
            findings: Vec::new(),
            run_fingerprint: "rf_1111111111111111".to_string(),
            change_fingerprint: None,
            access_problem: None,
        }
    }

    // ── Fixture-shape compatibility ─────────────────────────────────────

    #[test]
    fn update_detail_fixture_decodes_and_round_trips() {
        let update: MonitorUpdateDetailV1 =
            serde_json::from_str(UPDATE_FIXTURE).expect("fixture decodes as MonitorUpdateDetailV1");
        assert_eq!(update.update_id, "mu_fixture_0001");
        assert_eq!(update.status, MonitorRunStatus::Changed);
        assert_eq!(
            update.change_fingerprint.as_deref(),
            Some("chg_71d3f6a2c4e89b10")
        );
        assert_eq!(
            update.notification.policy,
            MonitorNotificationPolicy::MaterialChanges
        );
        assert!(update.notification.emitted);
        assert_eq!(
            update.notification.channel,
            MONITOR_UPDATE_CHANNEL_TODAY_CHANGED
        );
        assert_eq!(update.findings.len(), 2);

        let reserialized = serde_json::to_value(&update).expect("update serializes");
        let original: Value = serde_json::from_str(UPDATE_FIXTURE).expect("fixture is JSON");
        assert_eq!(
            reserialized, original,
            "MonitorUpdateDetailV1 must round-trip the canonical fixture without drift"
        );
    }

    #[test]
    fn fixture_dedupe_key_matches_the_canonical_composition() {
        // The fixture pins the EXACT §7.4 rendering.
        let update: MonitorUpdateDetailV1 =
            serde_json::from_str(UPDATE_FIXTURE).expect("fixture decodes");
        assert_eq!(
            notification_dedupe_key(
                "anonymous",
                "default",
                &update.monitor_task_id,
                update.monitor_revision,
                update.change_fingerprint.as_deref().expect("fingerprint"),
                &update.notification.channel,
            ),
            update.notification.dedupe_key
        );
    }

    // ── Policy matrix (§6.1 semantics, backend-owned §7.2) ──────────────

    #[test]
    fn material_change_under_default_policy_emits_with_material_findings() {
        let spec = fixture_spec(); // material_changes, no baseline opt-in
        let accepted = accepted_from(changed_result(), &spec);
        let update = build_monitor_update("anonymous", "default", &spec, &accepted)
            .expect("material run is update-worthy");
        assert!(update.notification.emitted);
        assert_eq!(update.status, MonitorRunStatus::Changed);
        assert_eq!(
            update.notification.dedupe_key,
            "anonymous/default:task_monitor_fixture_001:2:chg_71d3f6a2c4e89b10:today_changed",
            "dedupe key must be EXACTLY scope:task_id:revision:change_fingerprint:channel"
        );
        // Deterministic id derived from the dedupe key.
        assert!(update.update_id.starts_with("mu_"));
        assert_eq!(update.update_id.len(), "mu_".len() + 16);
        let again = build_monitor_update("anonymous", "default", &spec, &accepted).unwrap();
        assert_eq!(again.update_id, update.update_id);
        // Update findings are exactly the run's material findings.
        let material: Vec<_> = changed_result()
            .findings
            .into_iter()
            .filter(|finding| finding.classification.is_material())
            .collect();
        assert_eq!(update.findings, material);
        assert!(update_projects_to_changed(&update));
    }

    #[test]
    fn unchanged_run_under_default_policy_produces_no_update() {
        let spec = fixture_spec();
        let accepted = accepted_from(unchanged_result(), &spec);
        assert_eq!(
            build_monitor_update("anonymous", "default", &spec, &accepted),
            None,
            "quiet runs stay quiet (§3) — history lives on the Runs tab"
        );
    }

    #[test]
    fn never_policy_records_history_but_never_emits() {
        let mut spec = fixture_spec();
        spec.notification_policy = MonitorNotificationPolicy::Never;
        let accepted = accepted_from(changed_result(), &spec);
        assert!(!accepted.would_notify);
        let update = build_monitor_update("anonymous", "default", &spec, &accepted)
            .expect("material change still enters the Updates history");
        assert!(!update.notification.emitted);
        assert!(
            !update_projects_to_changed(&update),
            "policy never → no Today Changed card"
        );
    }

    #[test]
    fn every_run_policy_emits_a_receipt_even_on_unchanged_runs() {
        let mut spec = fixture_spec();
        spec.notification_policy = MonitorNotificationPolicy::EveryRun;
        let accepted = accepted_from(unchanged_result(), &spec);
        assert!(accepted.would_notify);
        let update = build_monitor_update("anonymous", "default", &spec, &accepted)
            .expect("every_run emits on unchanged too (§6.1)");
        assert!(update.notification.emitted);
        // No change fingerprint → the execution id keys the notification, so
        // each run emits once and retries of the SAME run collide.
        assert_eq!(
            update.notification.dedupe_key,
            "anonymous/default:task_monitor_fixture_001:2:exec_quiet_1:today_changed"
        );
        assert!(
            !update_projects_to_changed(&update),
            "quiet receipts stay off Today Changed (§3) — they live in Updates history"
        );

        // A different quiet run mints a different key/id (per-run receipts).
        let mut second = unchanged_result();
        second.execution_id = "exec_quiet_2".to_string();
        let second_update =
            build_monitor_update("anonymous", "default", &spec, &accepted_from(second, &spec))
                .unwrap();
        assert_ne!(second_update.update_id, update.update_id);
    }

    #[test]
    fn baseline_records_and_honors_the_opt_in() {
        let mut result = changed_result();
        result.status = MonitorRunStatus::Baseline;
        // Finalized baselines never carry a change fingerprint on the wire
        // (I3 coherence rule; `finalize_run_result`) — the §7.4 dedupe key
        // falls back to the execution id.
        result.change_fingerprint = None;
        for finding in &mut result.findings {
            finding.classification = super::super::monitor_run::MonitorFindingClassification::New;
        }

        // Quiet baseline (default): record exists, emitted=false, no card.
        let spec = fixture_spec();
        let mut accepted = accepted_from(result.clone(), &spec);
        accepted.material = false;
        accepted.would_notify = would_notify(&spec, MonitorRunStatus::Baseline, false);
        let quiet = build_monitor_update("anonymous", "default", &spec, &accepted)
            .expect("baselines always enter the Updates history");
        assert!(!quiet.notification.emitted);
        assert!(!update_projects_to_changed(&quiet));
        assert!(quiet.headline.starts_with("Baseline captured"));

        // Opted-in baseline: emitted and surfaced on Changed (§5.1 step 7).
        let mut opted = fixture_spec();
        opted.notify_initial_baseline = true;
        let mut accepted = accepted_from(result, &opted);
        accepted.material = false;
        accepted.would_notify = would_notify(&opted, MonitorRunStatus::Baseline, false);
        let loud = build_monitor_update("anonymous", "default", &opted, &accepted).unwrap();
        assert!(loud.notification.emitted);
        assert!(update_projects_to_changed(&loud));
    }

    #[test]
    fn degraded_runs_never_project_to_changed() {
        let mut spec = fixture_spec();
        spec.notification_policy = MonitorNotificationPolicy::EveryRun;
        let mut result = unchanged_result();
        result.status = MonitorRunStatus::Degraded;
        result.complete_scan = false;
        result.source_outcomes[0].status = MonitorSourceOutcomeStatus::AuthFailed;
        result.source_outcomes[0].complete = false;
        let accepted = accepted_from(result, &spec);
        let update = build_monitor_update("anonymous", "default", &spec, &accepted)
            .expect("every_run receipts include degraded runs");
        assert!(
            !update_projects_to_changed(&update),
            "degraded runs reach Attention (access problems), never Changed"
        );
    }

    // ── §12 suppression-reason mapping (bounded enum) ───────────────────

    #[test]
    fn suppression_reason_is_none_only_for_changed_surface_notifications() {
        // Material change under the default policy → notifies → None.
        let spec = fixture_spec();
        let accepted = accepted_from(changed_result(), &spec);
        let update = build_monitor_update("anonymous", "default", &spec, &accepted).unwrap();
        assert_eq!(
            notification_suppression_reason(&spec.notification_policy, Some(&update)),
            None
        );

        // Material change under `never` → recorded, suppressed policy_never.
        let mut never = fixture_spec();
        never.notification_policy = MonitorNotificationPolicy::Never;
        let accepted = accepted_from(changed_result(), &never);
        let update = build_monitor_update("anonymous", "default", &never, &accepted).unwrap();
        assert!(!update.notification.emitted);
        assert_eq!(
            notification_suppression_reason(&never.notification_policy, Some(&update)),
            Some(MonitorNotificationSuppressionReason::PolicyNever)
        );

        // Quiet run under the default policy → no record → unchanged.
        assert_eq!(
            notification_suppression_reason(&MonitorNotificationPolicy::MaterialChanges, None),
            Some(MonitorNotificationSuppressionReason::Unchanged)
        );
        // Quiet run under `never` → no record → policy_never.
        assert_eq!(
            notification_suppression_reason(&MonitorNotificationPolicy::Never, None),
            Some(MonitorNotificationSuppressionReason::PolicyNever)
        );

        // every_run receipt on an unchanged run: emitted into Updates
        // history but never a Changed card → quiet_every_run.
        let mut every = fixture_spec();
        every.notification_policy = MonitorNotificationPolicy::EveryRun;
        let accepted = accepted_from(unchanged_result(), &every);
        let receipt = build_monitor_update("anonymous", "default", &every, &accepted).unwrap();
        assert!(receipt.notification.emitted);
        assert!(!update_projects_to_changed(&receipt));
        assert_eq!(
            notification_suppression_reason(&every.notification_policy, Some(&receipt)),
            Some(MonitorNotificationSuppressionReason::QuietEveryRun)
        );

        // Quiet baseline without the opt-in → recorded, suppressed unchanged.
        let spec = fixture_spec();
        let mut baseline = changed_result();
        baseline.status = MonitorRunStatus::Baseline;
        baseline.change_fingerprint = None;
        let mut accepted = accepted_from(baseline, &spec);
        accepted.material = false;
        accepted.would_notify = would_notify(&spec, MonitorRunStatus::Baseline, false);
        let quiet = build_monitor_update("anonymous", "default", &spec, &accepted).unwrap();
        assert_eq!(
            notification_suppression_reason(&spec.notification_policy, Some(&quiet)),
            Some(MonitorNotificationSuppressionReason::Unchanged)
        );
    }

    #[test]
    fn suppression_reason_wire_tokens_are_the_bounded_enum() {
        assert_eq!(
            MonitorNotificationSuppressionReason::PolicyNever.as_str(),
            "policy_never"
        );
        assert_eq!(
            MonitorNotificationSuppressionReason::Unchanged.as_str(),
            "unchanged"
        );
        assert_eq!(
            MonitorNotificationSuppressionReason::DedupeReplay.as_str(),
            "dedupe_replay"
        );
        assert_eq!(
            MonitorNotificationSuppressionReason::QuietEveryRun.as_str(),
            "quiet_every_run"
        );
        // Serde emits the same tokens (enum stays bound to the wire).
        assert_eq!(
            serde_json::to_value(MonitorNotificationSuppressionReason::DedupeReplay).unwrap(),
            Value::String("dedupe_replay".to_string())
        );
    }

    // ── Access-problem escalation item ──────────────────────────────────

    #[test]
    fn access_problem_feed_item_is_deterministic_and_deep_linkable() {
        let problem = MonitorAccessProblemV1 {
            source: "https://dash.example/reports".to_string(),
            kind: MonitorSourceOutcomeStatus::AuthFailed,
            message: "Session expired — sign in again.".to_string(),
            since: "2026-07-21T06:01:00Z".to_string(),
        };
        let entry = MonitorSourceFailureEntry {
            source: problem.source.clone(),
            consecutive_failures: 2,
            last_status: MonitorSourceOutcomeStatus::AuthFailed,
            since: problem.since.clone(),
        };
        let item = monitor_access_problem_feed_item(
            "anonymous",
            "default",
            "task_monitor_1",
            "Dashboard monitor",
            &problem,
            &entry,
            "exec_run_2",
            Some("mu_abc"),
            1_800_000_000_000,
        );
        assert_eq!(
            item.id,
            monitor_access_problem_item_id("task_monitor_1", &problem.source),
            "id derives from task+source so upsert/resolve are idempotent"
        );
        assert_eq!(item.item_type, FeedItemType::Escalation);
        assert_eq!(item.status, FeedItemStatus::NeedsAction);
        assert_eq!(item.task_id.as_deref(), Some("task_monitor_1"));
        assert_eq!(item.metadata["monitor_task_id"], "task_monitor_1");
        assert_eq!(item.metadata["source"], problem.source);
        assert_eq!(item.metadata["execution_id"], "exec_run_2");
        assert_eq!(item.metadata["update_id"], "mu_abc");
        assert_eq!(item.metadata["consecutive_failures"], 2);
        assert_eq!(item.actions.len(), 1);
        assert_eq!(item.actions[0].id, "open_task");
        // Canonical monitor route incl. the update when known (C2).
        assert_eq!(
            item.actions[0].payload["url"],
            "/tasks?type=monitors&selected=task_monitor_1&update=mu_abc"
        );
        assert_eq!(
            item.metadata["deep_link"],
            "/tasks?type=monitors&selected=task_monitor_1&update=mu_abc"
        );
        // Same inputs → same id (restart-safe).
        assert_eq!(
            monitor_access_problem_item_id("task_monitor_1", &problem.source),
            item.id
        );
        // Different source → different id (per-source items).
        assert_ne!(
            monitor_access_problem_item_id("task_monitor_1", "https://other.example"),
            item.id
        );
    }
}
