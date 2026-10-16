//! One-way, human-facing work-ledger projection.
//!
//! Renders an agent's durable `work_outcome` [`EvidenceRecord`]s (the run-grained
//! ledger stamped by the work-ledger writer) into a deterministic Markdown
//! worklog for humans to read. This is a *projection FROM* the ledger: it reads
//! evidence and emits Markdown, nothing more. The rendered docs are **not**
//! agent-reachable — the memory index does not ingest `docs/`, and nothing here
//! ever reads a rendered worklog back. The CLI `worklog-export` subcommand writes
//! the output of [`render_agent_worklog_markdown`] to `docs/worklog/<agent>.md`.
//!
//! Determinism: the render is a pure function of the input records (filter to the
//! `work_outcome` producer, sort newest-first by `last_seen_at`, stable emit). No
//! clock, no randomness — the same records always produce byte-identical output,
//! so unit tests can assert on the exact bytes.

use super::EvidenceRecord;

/// Producer tag for the deterministic run-grained work-ledger lane (see
/// [`EvidenceRecord::from_work_outcome`]).
const WORK_OUTCOME_PRODUCER: &str = "work_outcome";

/// Pull a string field out of the record's open `metadata` bag, trimmed;
/// `None` when absent, non-string, or empty after trimming.
fn metadata_str(record: &EvidenceRecord, key: &str) -> Option<String> {
    record
        .metadata
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Pull a string array out of the record's open `metadata` bag; each element is
/// trimmed and empties are dropped. Returns an empty vec when absent / not an
/// array.
fn metadata_str_array(record: &EvidenceRecord, key: &str) -> Vec<String> {
    record
        .metadata
        .get(key)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Render one agent's `work_outcome` evidence into a deterministic Markdown
/// worklog.
///
/// Filters `records` to the `work_outcome` producer lane, sorts newest-first by
/// `last_seen_at` (RFC3339, so lexicographic == chronological; ties break on
/// `evidence_id` for a total order), and emits a title plus one dated section per
/// record with its outcome, summary, open loops (when any), next-step hint (when
/// present), and artifact refs (when any).
///
/// Pure and deterministic: no clock, no randomness. The same `records` always
/// produce byte-identical output.
pub fn render_agent_worklog_markdown(agent_id: &str, records: &[EvidenceRecord]) -> String {
    let mut entries: Vec<&EvidenceRecord> = records
        .iter()
        .filter(|record| record.producer == WORK_OUTCOME_PRODUCER)
        .collect();
    // Newest-first by last_seen_at; evidence_id tie-break for a total, stable order.
    entries.sort_by(|a, b| {
        b.last_seen_at
            .cmp(&a.last_seen_at)
            .then_with(|| b.evidence_id.cmp(&a.evidence_id))
    });

    let mut out = String::new();
    out.push_str(&format!("# Worklog — {agent_id}\n\n"));
    out.push_str(
        "One-way human-readable projection of this agent's work ledger. Generated from \
         evidence records; do not edit — regenerate via `magician worklog-export`.\n\n",
    );

    if entries.is_empty() {
        out.push_str("_No work-outcome records yet._\n");
        return out;
    }

    for record in entries {
        let outcome = metadata_str(record, "outcome").unwrap_or_else(|| "unknown".to_string());
        // Heading: dated, outcome-tagged. `last_seen_at` is the run's completion
        // stamp (RFC3339); rendered verbatim so the output stays clock-free.
        out.push_str(&format!("## {} — {}\n\n", record.last_seen_at, outcome));

        let summary = record.summary.trim();
        if !summary.is_empty() {
            out.push_str(summary);
            out.push_str("\n\n");
        }

        if let Some(hint) = metadata_str(record, "next_step_hint") {
            out.push_str(&format!("- Next step: {hint}\n"));
        }

        let open_loops = metadata_str_array(record, "open_loops");
        if !open_loops.is_empty() {
            out.push_str("- Open loops:\n");
            for loop_item in &open_loops {
                out.push_str(&format!("  - {loop_item}\n"));
            }
        }

        if !record.artifact_refs.is_empty() {
            out.push_str("- Artifacts:\n");
            for artifact in &record.artifact_refs {
                out.push_str(&format!("  - {artifact}\n"));
            }
        }

        out.push_str(&format!("- Evidence: `{}`\n\n", record.evidence_id));
    }

    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::evidence::{EvidenceRecord, WorkOutcomeInput};

    fn work_record(
        root_execution_id: &str,
        ts_ms: i64,
        summary: &str,
        outcome: &str,
    ) -> EvidenceRecord {
        EvidenceRecord::from_work_outcome(WorkOutcomeInput {
            root_execution_id: root_execution_id.to_string(),
            task_id: Some(format!("task-{root_execution_id}")),
            agent_id: "personal-assistant".to_string(),
            outcome: outcome.to_string(),
            summary: summary.to_string(),
            artifacts: vec![format!("artifact://{root_execution_id}/out.md")],
            open_loops: vec![format!("follow up on {root_execution_id}")],
            next_step_hint: Some(format!("do next for {root_execution_id}")),
            entity_keys: vec![],
            timestamp_ms: ts_ms,
        })
    }

    /// A non-work-outcome record (task-episode distillation lane) must be excluded
    /// from the worklog even though it lives in the same evidence collection.
    fn non_work_record() -> EvidenceRecord {
        let mut record = work_record(
            "should-not-appear",
            5_000,
            "distilled activity note",
            "success",
        );
        record.producer = "task_episode".to_string();
        record.evidence_id = "evd:episode:xyz".to_string();
        record.summary = "SENTINEL-EXCLUDED-SUMMARY".to_string();
        record
    }

    #[test]
    fn worklog_filters_sorts_and_is_byte_stable() {
        // Two work records (out of chronological order on purpose) + one non-work.
        let older = work_record("run-A", 1_000, "Booked the flights", "success");
        let newer = work_record("run-B", 9_000, "Drafted the quarterly report", "yield");
        let records = vec![older, non_work_record(), newer];

        let markdown = render_agent_worklog_markdown("personal-assistant", &records);

        // Title present.
        assert!(markdown.contains("# Worklog — personal-assistant"));

        // Both work entries present.
        assert!(markdown.contains("Drafted the quarterly report"));
        assert!(markdown.contains("Booked the flights"));

        // Newest-first: run-B (ts 9000) appears before run-A (ts 1000).
        let idx_newer = markdown.find("Drafted the quarterly report").unwrap();
        let idx_older = markdown.find("Booked the flights").unwrap();
        assert!(
            idx_newer < idx_older,
            "newest work_outcome must render before older one"
        );

        // The non-work record is excluded.
        assert!(
            !markdown.contains("SENTINEL-EXCLUDED-SUMMARY"),
            "task_episode producer must be filtered out of the worklog"
        );
        assert!(!markdown.contains("evd:episode:xyz"));

        // Outcomes, open loops, next step, and artifacts render.
        assert!(markdown.contains("— yield"));
        assert!(markdown.contains("— success"));
        assert!(markdown.contains("Open loops:"));
        assert!(markdown.contains("follow up on run-B"));
        assert!(markdown.contains("Next step: do next for run-B"));
        assert!(markdown.contains("artifact://run-B/out.md"));
        assert!(markdown.contains("`evd:run:run-B`"));

        // Byte-stable: rendering the same records again is identical.
        let again = render_agent_worklog_markdown("personal-assistant", &records);
        assert_eq!(
            markdown, again,
            "render must be deterministic / byte-stable"
        );
    }

    #[test]
    fn worklog_empty_when_no_work_outcome_records() {
        let markdown = render_agent_worklog_markdown("empty-agent", &[non_work_record()]);
        assert!(markdown.contains("# Worklog — empty-agent"));
        assert!(markdown.contains("_No work-outcome records yet._"));
        assert!(!markdown.contains("SENTINEL-EXCLUDED-SUMMARY"));
    }
}
