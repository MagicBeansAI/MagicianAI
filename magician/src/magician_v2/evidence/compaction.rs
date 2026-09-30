//! Evidence compaction (WEG generic substrate — daily window).
//!
//! Different episodes about the same entity + same observed-action-kind on the
//! same day distill into distinct `evidence_id`s and would otherwise accumulate
//! until the retention cap truncates *by recency* (silently dropping
//! older-but-valid items). This pass conservatively merges those duplicates into
//! one record per `(entity-set, evidence_kind, day)` so the collection stays
//! small without losing distinct, high-salience actions.
//!
//! Invariants (deliberately conservative — this runs on every append):
//! - user-corrected records (suppressed / deleted / user-assigned facet) are
//!   NEVER merged or dropped — [`EvidenceRecord::is_user_corrected`] items pass
//!   through untouched;
//! - work-outcome ledger records (`producer == "work_outcome"`) are NEVER merged
//!   — each run is a distinct unit of work, so ledger rows always stay solo;
//! - distinct `evidence_kind`s on the same entity/day stay separate (a "reviewed"
//!   and a "submitted" are not collapsed into one);
//! - anchorless records (empty `entity_keys`) never merge with anything;
//! - sensitivity is **most-restrictive**: if any member is sensitive, the merged
//!   record is sensitive (merging must never launder a sensitive item out of the
//!   suppression set);
//! - deterministic + idempotent: compacting an already-compacted list is a no-op.

use std::collections::HashMap;

use chrono::DateTime;

use super::{is_sensitive, EvidenceRecord, Facet};

/// Result of a compaction pass.
#[derive(Debug, Clone, Default)]
pub struct CompactionOutcome {
    /// Compacted records, newest (`last_seen_at`) first, deterministic order.
    pub records: Vec<EvidenceRecord>,
    /// Records eliminated by merging (input len − output len).
    pub merged: usize,
}

/// Merge same-`(entity-set, evidence_kind, day)` duplicate evidence. See module docs.
pub fn compact_evidence(records: Vec<EvidenceRecord>) -> CompactionOutcome {
    let input_len = records.len();

    let mut groups: HashMap<String, Vec<EvidenceRecord>> = HashMap::new();
    for record in records {
        groups.entry(group_key(&record)).or_default().push(record);
    }

    let mut out: Vec<EvidenceRecord> = Vec::with_capacity(groups.len());
    for (_key, group) in groups {
        if group.len() <= 1 {
            out.extend(group);
        } else {
            out.push(merge_group(group));
        }
    }

    // Total deterministic order: newest first, `evidence_id` breaks ties.
    out.sort_by(|a, b| {
        epoch_ms(&b.last_seen_at)
            .cmp(&epoch_ms(&a.last_seen_at))
            .then_with(|| a.evidence_id.cmp(&b.evidence_id))
    });

    let merged = input_len.saturating_sub(out.len());
    CompactionOutcome {
        records: out,
        merged,
    }
}

/// Records merge only when they share the same NON-EMPTY entity set, the same
/// `evidence_kind`, and the same calendar day. User-corrected and anchorless
/// records get a unique key (per `evidence_id`) so they never merge.
fn group_key(record: &EvidenceRecord) -> String {
    if record.is_user_corrected() || is_ledger_record(record) || record.entity_keys.is_empty() {
        return format!("\u{0}solo\u{0}{}", record.evidence_id);
    }
    let mut keys: Vec<&str> = record.entity_keys.iter().map(String::as_str).collect();
    keys.sort_unstable();
    keys.dedup();
    format!(
        "{}\u{0}{}\u{0}{}",
        keys.join(","),
        record.evidence_kind,
        day_bucket(&record.last_seen_at)
    )
}

/// Work-outcome ledger records represent one completed run each, so they must
/// never cross-run merge (unlike ambient activity evidence). Detected purely by
/// producer lane so the bypass stays targeted to ledger rows.
fn is_ledger_record(r: &EvidenceRecord) -> bool {
    r.producer == "work_outcome"
}

fn merge_group(group: Vec<EvidenceRecord>) -> EvidenceRecord {
    // Newest record (tie: `evidence_id`) is the base for id / kind / summary /
    // producer; the rest contribute unions and extremes.
    let newest = group
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            epoch_ms(&a.last_seen_at)
                .cmp(&epoch_ms(&b.last_seen_at))
                .then_with(|| a.evidence_id.cmp(&b.evidence_id))
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut base = group[newest].clone();

    let mut observed = Vec::new();
    let mut entities = Vec::new();
    let mut people = Vec::new();
    let mut artifacts = Vec::new();
    let mut sources = Vec::new();
    let mut facets: Vec<Facet> = Vec::new();
    let mut first_seen = base.first_seen_at.clone();
    let mut last_seen = base.last_seen_at.clone();
    let mut sensitive: Option<String> = None;

    for record in &group {
        push_unique(&mut observed, &record.observed_actions);
        push_unique(&mut entities, &record.entity_keys);
        push_unique(&mut people, &record.people_keys);
        push_unique(&mut artifacts, &record.artifact_refs);
        push_unique(&mut sources, &record.source_refs);
        merge_facets(&mut facets, &record.facets);
        base.importance = base.importance.max(record.importance);
        base.confidence = base.confidence.max(record.confidence);
        if epoch_ms(&record.first_seen_at) < epoch_ms(&first_seen) {
            first_seen = record.first_seen_at.clone();
        }
        if epoch_ms(&record.last_seen_at) > epoch_ms(&last_seen) {
            last_seen = record.last_seen_at.clone();
        }
        if sensitive.is_none() && is_sensitive(&record.sensitivity) {
            sensitive = Some(record.sensitivity.clone());
        }
    }

    base.observed_actions = observed;
    base.entity_keys = entities;
    base.people_keys = people;
    base.artifact_refs = artifacts;
    base.source_refs = sources;
    base.facets = facets;
    base.first_seen_at = first_seen;
    base.last_seen_at = last_seen;
    if let Some(label) = sensitive {
        base.sensitivity = label;
    }
    base
}

fn push_unique(dst: &mut Vec<String>, src: &[String]) {
    for value in src {
        if !dst.iter().any(|existing| existing == value) {
            dst.push(value.clone());
        }
    }
}

fn merge_facets(dst: &mut Vec<Facet>, src: &[Facet]) {
    for facet in src {
        if let Some(existing) = dst.iter_mut().find(|e| e.label == facet.label) {
            if facet.confidence > existing.confidence {
                existing.confidence = facet.confidence;
                existing.assigned_by = facet.assigned_by.clone();
            }
        } else {
            dst.push(facet.clone());
        }
    }
}

fn day_bucket(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| timestamp.to_string())
}

fn epoch_ms(timestamp: &str) -> i64 {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::evidence::EvidenceStatus;

    fn rec(id: &str, kind: &str, entities: &[&str], last_seen: &str) -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: id.to_string(),
            summary: format!("did {kind}"),
            evidence_kind: kind.to_string(),
            observed_actions: vec![kind.to_string()],
            entity_keys: entities.iter().map(|s| s.to_string()).collect(),
            people_keys: Vec::new(),
            artifact_refs: Vec::new(),
            source_refs: vec![format!("episode:{id}")],
            facets: vec![Facet {
                label: "work".into(),
                confidence: 0.5,
                assigned_by: "llm".into(),
            }],
            importance: 0.5,
            confidence: 0.5,
            sensitivity: "work".into(),
            first_seen_at: last_seen.to_string(),
            last_seen_at: last_seen.to_string(),
            status: EvidenceStatus::Active,
            last_corrected_at: None,
            producer: "task_episode".into(),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn merges_same_entity_kind_day_and_unions_refs() {
        let mut a = rec("evd:1", "view", &["pr:481"], "2026-06-13T09:00:00Z");
        a.importance = 0.6;
        let b = rec("evd:2", "view", &["pr:481"], "2026-06-13T11:00:00Z");
        let out = compact_evidence(vec![a, b]);
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.merged, 1);
        let m = &out.records[0];
        assert_eq!(m.evidence_id, "evd:2", "newest id wins");
        assert!(
            (m.importance - 0.6).abs() < f64::EPSILON,
            "importance = max"
        );
        assert_eq!(m.source_refs.len(), 2, "source refs unioned");
        assert_eq!(
            m.first_seen_at, "2026-06-13T09:00:00Z",
            "earliest first_seen"
        );
        assert_eq!(m.last_seen_at, "2026-06-13T11:00:00Z", "latest last_seen");
    }

    #[test]
    fn keeps_distinct_kinds_separate() {
        let a = rec("evd:1", "reviewed", &["pr:481"], "2026-06-13T09:00:00Z");
        let b = rec("evd:2", "submitted", &["pr:481"], "2026-06-13T10:00:00Z");
        let out = compact_evidence(vec![a, b]);
        assert_eq!(
            out.records.len(),
            2,
            "different evidence_kind must not merge"
        );
        assert_eq!(out.merged, 0);
    }

    #[test]
    fn keeps_different_days_separate() {
        let a = rec("evd:1", "view", &["pr:481"], "2026-06-13T23:00:00Z");
        let b = rec("evd:2", "view", &["pr:481"], "2026-06-14T01:00:00Z");
        let out = compact_evidence(vec![a, b]);
        assert_eq!(out.records.len(), 2, "different day must not merge");
    }

    #[test]
    fn never_merges_user_corrected() {
        let a = rec("evd:1", "view", &["pr:481"], "2026-06-13T09:00:00Z");
        let mut b = rec("evd:2", "view", &["pr:481"], "2026-06-13T11:00:00Z");
        b.status = EvidenceStatus::Suppressed; // user-corrected
        let out = compact_evidence(vec![a, b]);
        assert_eq!(
            out.records.len(),
            2,
            "suppressed record passes through untouched"
        );
        assert!(out
            .records
            .iter()
            .any(|r| r.status == EvidenceStatus::Suppressed));
    }

    #[test]
    fn never_merges_anchorless() {
        let a = rec("evd:1", "view", &[], "2026-06-13T09:00:00Z");
        let b = rec("evd:2", "view", &[], "2026-06-13T11:00:00Z");
        let out = compact_evidence(vec![a, b]);
        assert_eq!(
            out.records.len(),
            2,
            "anchorless (no entity_keys) never merges"
        );
    }

    #[test]
    fn sensitivity_is_most_restrictive() {
        let a = rec("evd:1", "view", &["doc:x"], "2026-06-13T09:00:00Z"); // work
        let mut b = rec("evd:2", "view", &["doc:x"], "2026-06-13T11:00:00Z");
        b.sensitivity = "financial".into(); // sensitive
        let out = compact_evidence(vec![a, b]);
        assert_eq!(out.records.len(), 1);
        assert_eq!(
            out.records[0].sensitivity, "financial",
            "sensitive flag must survive the merge"
        );
        assert!(is_sensitive(&out.records[0].sensitivity));
    }

    #[test]
    fn never_merges_ledger_records() {
        // Two work-outcome ledger records identical in (entity-set, kind, day)
        // must stay solo — each run is its own distinct unit of work.
        let mut a = rec("evd:run:1", "shipped", &["pr:481"], "2026-06-13T09:00:00Z");
        a.producer = "work_outcome".to_string();
        let mut b = rec("evd:run:2", "shipped", &["pr:481"], "2026-06-13T11:00:00Z");
        b.producer = "work_outcome".to_string();
        let out = compact_evidence(vec![a, b]);
        assert_eq!(
            out.records.len(),
            2,
            "work_outcome ledger records must never cross-run merge"
        );
        assert_eq!(out.merged, 0);
    }

    #[test]
    fn control_non_ledger_records_still_merge() {
        // CONTROL: identical to `never_merges_ledger_records` except the producer
        // is a normal (non-ledger) lane — these MUST still merge, proving the
        // bypass is targeted to ledger records only.
        let mut a = rec("evd:run:1", "shipped", &["pr:481"], "2026-06-13T09:00:00Z");
        a.producer = "task_episode".to_string();
        let mut b = rec("evd:run:2", "shipped", &["pr:481"], "2026-06-13T11:00:00Z");
        b.producer = "task_episode".to_string();
        let out = compact_evidence(vec![a, b]);
        assert_eq!(
            out.records.len(),
            1,
            "non-ledger records with same (entity-set, kind, day) still merge"
        );
        assert_eq!(out.merged, 1);
    }

    #[test]
    fn idempotent() {
        let records = vec![
            rec("evd:1", "view", &["pr:481"], "2026-06-13T09:00:00Z"),
            rec("evd:2", "view", &["pr:481"], "2026-06-13T11:00:00Z"),
            rec("evd:3", "submitted", &["pr:481"], "2026-06-13T12:00:00Z"),
        ];
        let once = compact_evidence(records);
        let twice = compact_evidence(once.records.clone());
        assert_eq!(once.records.len(), twice.records.len());
        assert_eq!(twice.merged, 0, "re-compaction merges nothing");
    }
}
