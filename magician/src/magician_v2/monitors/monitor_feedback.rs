//! Recurring Monitors (Phase 6) — useful / not-relevant feedback records.
//!
//! Plan: `docs/plans/2026-07-21-recurring-monitors-productization-design-implementation.md`
//! §10 (Feedback And Trust): feedback never silently rewrites the monitor
//! contract in v1 — a `not_relevant` verdict RETAINS the implicated finding
//! identities (stable keys + content fingerprints) as EVIDENCE, copied from
//! the update record at write time, so a later previewed rule/threshold
//! suggestion can cite exactly what the user rejected.
//!
//! Everything in this module is pure and deterministic — no LLM, no I/O, no
//! clock. `ArtifactV2Service::record_monitor_update_feedback` owns
//! persistence (the per-task `monitor_feedback.jsonl` ledger beside
//! `monitor_updates.jsonl`, same bounded-retention discipline) and calls
//! into these builders.
//!
//! Wire contract (fixed — clients are built against exactly this):
//!
//! ```text
//! POST /api/magician/v3/monitors/{task_id}/updates/{update_id}/feedback
//!   {"verdict": "useful" | "not_relevant", "note": optional ≤500 chars}
//!   200 {"task_id","update_id","verdict","recorded":bool,"feedback_id":"mf_<hash>"}
//! GET  /api/magician/v3/monitors/{task_id}/feedback?limit=
//!   {items:[{feedback_id,update_id,verdict,note?,recorded_at}], next_cursor:null, limit}
//! ```
//!
//! Idempotency: `feedback_id` is a deterministic blake3 over
//! `scope:task:update:verdict`, so re-posting the SAME verdict for an update
//! replays the existing record (`recorded: false`, same id) while posting
//! the OTHER verdict appends a new record — LATEST WINS per update. Flipping
//! back re-derives the ORIGINAL id (determinism over novelty); the ledger
//! keeps the full append-only history, the read path serves the latest
//! verdict per update.

use serde::{Deserialize, Serialize};

use super::monitor_updates::MonitorUpdateDetailV1;

/// Feedback note cap (wire contract: "note: optional string ≤500"). Longer
/// notes are truncated at admission rather than rejected — the contract
/// defines no oversized-note error and parallel clients only handle the
/// documented 400/404 bodies.
pub const MONITOR_FEEDBACK_NOTE_MAX_CHARS: usize = 500;

/// The two §10 verdicts. Wire tokens are the snake_case serde names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorFeedbackVerdict {
    Useful,
    NotRelevant,
}

impl MonitorFeedbackVerdict {
    /// Stable wire token — used inside the deterministic `feedback_id`
    /// payload so the hash never depends on serde internals.
    pub fn as_str(&self) -> &'static str {
        match self {
            MonitorFeedbackVerdict::Useful => "useful",
            MonitorFeedbackVerdict::NotRelevant => "not_relevant",
        }
    }

    /// Parse the wire token. Anything else is the contract's 400
    /// `monitor_feedback_verdict_invalid`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "useful" => Some(MonitorFeedbackVerdict::Useful),
            "not_relevant" => Some(MonitorFeedbackVerdict::NotRelevant),
            _ => None,
        }
    }
}

/// Evidence retained with a `not_relevant` verdict (§10: "It is retained as
/// evidence and can inform a previewed rule/threshold suggestion later").
/// Copied VERBATIM from the update record at write time — never re-derived
/// later, so the evidence survives update-ledger compaction and monitor
/// edits without drifting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorFeedbackEvidenceV1 {
    /// The run that produced the disputed update.
    pub execution_id: String,
    /// The update's change fingerprint, when it had one (§7.4 continuity —
    /// lets a suggestion engine correlate the rejection with the exact
    /// deduped change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_fingerprint: Option<String>,
    /// Stable keys of the findings the user implicated as not relevant.
    pub stable_keys: Vec<String>,
    /// Content fingerprints of those findings (parallel to `stable_keys`).
    pub content_fingerprints: Vec<String>,
}

/// One durable feedback record — one line of the per-task
/// `monitor_feedback.jsonl` ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorUpdateFeedbackV1 {
    /// `mf_<16-hex blake3 of scope:task:update:verdict>` — deterministic, so
    /// a replayed POST re-derives the SAME id and the ledger append is
    /// skipped (`recorded: false` on the wire).
    pub feedback_id: String,
    pub monitor_task_id: String,
    pub update_id: String,
    pub verdict: MonitorFeedbackVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub recorded_at: String,
    /// Present only on `not_relevant` verdicts (§10 evidence retention).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<MonitorFeedbackEvidenceV1>,
}

/// Deterministic feedback id: `mf_<16-hex blake3>` over the fixed payload
/// `principal/workspace:task_id:update_id:verdict` (scope rendered exactly
/// like the §7.4 dedupe key's scope component).
pub fn monitor_feedback_id(
    principal: &str,
    workspace: &str,
    task_id: &str,
    update_id: &str,
    verdict: MonitorFeedbackVerdict,
) -> String {
    let payload = format!(
        "{principal}/{workspace}:{task_id}:{update_id}:{}",
        verdict.as_str()
    );
    format!("mf_{}", &blake3::hash(payload.as_bytes()).to_hex()[..16])
}

/// Truncate a feedback note to the wire bound (char-counted, like the
/// Phase 1 admission caps). Empty/whitespace-only notes collapse to `None`.
pub fn normalize_feedback_note(note: Option<String>) -> Option<String> {
    let note = note?;
    let trimmed = note.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .chars()
            .take(MONITOR_FEEDBACK_NOTE_MAX_CHARS)
            .collect(),
    )
}

/// Build the deterministic feedback record for one update. Pure — the caller
/// owns `recorded_at` (the service stamps `now_rfc3339()`) and persistence.
///
/// §10 evidence capture: `not_relevant` copies the update's finding
/// identities (stable keys + content fingerprints) and change fingerprint at
/// write time; `useful` records no evidence block (nothing is disputed).
pub fn build_monitor_feedback(
    principal: &str,
    workspace: &str,
    update: &MonitorUpdateDetailV1,
    verdict: MonitorFeedbackVerdict,
    note: Option<String>,
    recorded_at: String,
) -> MonitorUpdateFeedbackV1 {
    let evidence = match verdict {
        MonitorFeedbackVerdict::Useful => None,
        MonitorFeedbackVerdict::NotRelevant => Some(MonitorFeedbackEvidenceV1 {
            execution_id: update.execution_id.clone(),
            change_fingerprint: update.change_fingerprint.clone(),
            stable_keys: update
                .findings
                .iter()
                .map(|finding| finding.stable_key.clone())
                .collect(),
            content_fingerprints: update
                .findings
                .iter()
                .map(|finding| finding.content_fingerprint.clone())
                .collect(),
        }),
    };
    MonitorUpdateFeedbackV1 {
        feedback_id: monitor_feedback_id(
            principal,
            workspace,
            &update.monitor_task_id,
            &update.update_id,
            verdict,
        ),
        monitor_task_id: update.monitor_task_id.clone(),
        update_id: update.update_id.clone(),
        verdict,
        note: normalize_feedback_note(note),
        recorded_at,
        evidence,
    }
}

/// Fold an append-only feedback ledger down to the CURRENT verdict per
/// update (latest wins — a flipped verdict replaces the earlier one), newest
/// first by `recorded_at` (then `feedback_id` for a total order). Ledger
/// order is authoritative for "latest": the last line for an update id is
/// its current verdict.
pub fn latest_feedback_per_update(
    records: Vec<MonitorUpdateFeedbackV1>,
) -> Vec<MonitorUpdateFeedbackV1> {
    let mut latest: Vec<MonitorUpdateFeedbackV1> = Vec::new();
    for record in records {
        if let Some(existing) = latest
            .iter_mut()
            .find(|existing| existing.update_id == record.update_id)
        {
            *existing = record;
        } else {
            latest.push(record);
        }
    }
    latest.sort_by(|left, right| {
        right
            .recorded_at
            .cmp(&left.recorded_at)
            .then_with(|| right.feedback_id.cmp(&left.feedback_id))
    });
    latest
}

/// Outcome of one feedback POST — the persisted (or replayed) record plus
/// whether THIS call appended it (`recorded` on the wire).
#[derive(Debug, Clone)]
pub struct MonitorFeedbackOutcome {
    pub feedback: MonitorUpdateFeedbackV1,
    pub newly_recorded: bool,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::monitor_run::MonitorRunStatus;
    use super::super::monitor_spec::MonitorNotificationPolicy;
    use super::super::monitor_updates::{MonitorUpdateDetailV1, MonitorUpdateNotificationV1};
    use super::*;

    const UPDATE_FIXTURE: &str =
        include_str!("../../../tests/fixtures/monitors/monitor_update_detail_v1.json");

    fn fixture_update() -> MonitorUpdateDetailV1 {
        serde_json::from_str(UPDATE_FIXTURE).expect("update fixture decodes")
    }

    #[test]
    fn feedback_id_is_deterministic_and_component_sensitive() {
        let id = monitor_feedback_id(
            "anonymous",
            "default",
            "task_1",
            "mu_abc",
            MonitorFeedbackVerdict::Useful,
        );
        assert!(id.starts_with("mf_"));
        assert_eq!(id.len(), "mf_".len() + 16);
        // Same inputs → same id (replay returns the identical id).
        assert_eq!(
            id,
            monitor_feedback_id(
                "anonymous",
                "default",
                "task_1",
                "mu_abc",
                MonitorFeedbackVerdict::Useful
            )
        );
        // Every component perturbs the hash.
        for other in [
            monitor_feedback_id(
                "other",
                "default",
                "task_1",
                "mu_abc",
                MonitorFeedbackVerdict::Useful,
            ),
            monitor_feedback_id(
                "anonymous",
                "other",
                "task_1",
                "mu_abc",
                MonitorFeedbackVerdict::Useful,
            ),
            monitor_feedback_id(
                "anonymous",
                "default",
                "task_2",
                "mu_abc",
                MonitorFeedbackVerdict::Useful,
            ),
            monitor_feedback_id(
                "anonymous",
                "default",
                "task_1",
                "mu_def",
                MonitorFeedbackVerdict::Useful,
            ),
            monitor_feedback_id(
                "anonymous",
                "default",
                "task_1",
                "mu_abc",
                MonitorFeedbackVerdict::NotRelevant,
            ),
        ] {
            assert_ne!(id, other);
        }
    }

    #[test]
    fn verdict_parses_only_the_two_wire_tokens() {
        assert_eq!(
            MonitorFeedbackVerdict::parse("useful"),
            Some(MonitorFeedbackVerdict::Useful)
        );
        assert_eq!(
            MonitorFeedbackVerdict::parse(" not_relevant "),
            Some(MonitorFeedbackVerdict::NotRelevant)
        );
        for invalid in ["Useful", "NOT_RELEVANT", "irrelevant", "", "maybe"] {
            assert_eq!(MonitorFeedbackVerdict::parse(invalid), None, "{invalid}");
        }
        // Serde wire round-trip matches as_str.
        assert_eq!(
            serde_json::to_value(MonitorFeedbackVerdict::NotRelevant).unwrap(),
            serde_json::json!("not_relevant")
        );
    }

    #[test]
    fn not_relevant_copies_finding_evidence_at_write_time() {
        let update = fixture_update();
        let record = build_monitor_feedback(
            "anonymous",
            "default",
            &update,
            MonitorFeedbackVerdict::NotRelevant,
            Some("wrong product line".to_string()),
            "2026-07-22T08:00:00Z".to_string(),
        );
        let evidence = record
            .evidence
            .as_ref()
            .expect("not_relevant keeps evidence");
        assert_eq!(evidence.execution_id, update.execution_id);
        assert_eq!(evidence.change_fingerprint, update.change_fingerprint);
        assert_eq!(
            evidence.stable_keys,
            update
                .findings
                .iter()
                .map(|f| f.stable_key.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            evidence.content_fingerprints,
            update
                .findings
                .iter()
                .map(|f| f.content_fingerprint.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(record.note.as_deref(), Some("wrong product line"));
        assert_eq!(record.update_id, update.update_id);
        assert_eq!(record.monitor_task_id, update.monitor_task_id);

        // Useful verdicts dispute nothing — no evidence block.
        let useful = build_monitor_feedback(
            "anonymous",
            "default",
            &update,
            MonitorFeedbackVerdict::Useful,
            None,
            "2026-07-22T08:00:00Z".to_string(),
        );
        assert!(useful.evidence.is_none());
        assert!(useful.note.is_none());
        assert_ne!(useful.feedback_id, record.feedback_id);
    }

    #[test]
    fn note_is_bounded_at_500_chars_and_blank_collapses_to_none() {
        assert_eq!(normalize_feedback_note(None), None);
        assert_eq!(normalize_feedback_note(Some("   ".to_string())), None);
        assert_eq!(
            normalize_feedback_note(Some("  fine  ".to_string())).as_deref(),
            Some("fine")
        );
        let long = "x".repeat(MONITOR_FEEDBACK_NOTE_MAX_CHARS + 50);
        let bounded = normalize_feedback_note(Some(long)).expect("kept");
        assert_eq!(bounded.chars().count(), MONITOR_FEEDBACK_NOTE_MAX_CHARS);
    }

    #[test]
    fn latest_wins_per_update_and_sorts_newest_first() {
        let update = fixture_update();
        let mut other = fixture_update();
        other.update_id = "mu_other_0002".to_string();

        // Ledger order: useful(update), useful(other), not_relevant(update).
        let first = build_monitor_feedback(
            "anonymous",
            "default",
            &update,
            MonitorFeedbackVerdict::Useful,
            None,
            "2026-07-22T08:00:00Z".to_string(),
        );
        let second = build_monitor_feedback(
            "anonymous",
            "default",
            &other,
            MonitorFeedbackVerdict::Useful,
            None,
            "2026-07-22T09:00:00Z".to_string(),
        );
        let flipped = build_monitor_feedback(
            "anonymous",
            "default",
            &update,
            MonitorFeedbackVerdict::NotRelevant,
            None,
            "2026-07-22T10:00:00Z".to_string(),
        );
        let current =
            latest_feedback_per_update(vec![first.clone(), second.clone(), flipped.clone()]);
        assert_eq!(current.len(), 2, "one CURRENT verdict per update");
        // Newest first: the flip (10:00) leads, then the other update (09:00).
        assert_eq!(current[0].feedback_id, flipped.feedback_id);
        assert_eq!(current[0].verdict, MonitorFeedbackVerdict::NotRelevant);
        assert_eq!(current[1].feedback_id, second.feedback_id);
        // The replaced useful record is gone from the current view.
        assert!(current.iter().all(|r| r.feedback_id != first.feedback_id));

        // Flipping BACK re-derives the original deterministic id.
        let back = build_monitor_feedback(
            "anonymous",
            "default",
            &update,
            MonitorFeedbackVerdict::Useful,
            None,
            "2026-07-22T11:00:00Z".to_string(),
        );
        assert_eq!(back.feedback_id, first.feedback_id);
    }

    #[test]
    fn feedback_record_round_trips_and_omits_optional_fields() {
        let update = fixture_update();
        let record = build_monitor_feedback(
            "anonymous",
            "default",
            &update,
            MonitorFeedbackVerdict::Useful,
            None,
            "2026-07-22T08:00:00Z".to_string(),
        );
        let value = serde_json::to_value(&record).expect("serializes");
        // Optional fields are OMITTED, not null (jsonl ledger stays compact).
        assert!(value.get("note").is_none());
        assert!(value.get("evidence").is_none());
        let decoded: MonitorUpdateFeedbackV1 = serde_json::from_value(value).expect("round-trips");
        assert_eq!(decoded, record);
    }

    /// Keeps this module honest against fixture drift: the evidence copy
    /// reads real fixture findings (a `changed` update with 2 findings and
    /// a change fingerprint under the material_changes policy).
    #[test]
    fn fixture_update_shape_backs_the_evidence_contract() {
        let update = fixture_update();
        assert_eq!(update.status, MonitorRunStatus::Changed);
        assert_eq!(update.findings.len(), 2);
        assert!(update.change_fingerprint.is_some());
        assert_eq!(
            update.notification.policy,
            MonitorNotificationPolicy::MaterialChanges
        );
        let _: &MonitorUpdateNotificationV1 = &update.notification;
    }
}
