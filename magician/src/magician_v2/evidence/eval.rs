//! Deterministic eval harness for the work-evidence graph (Slice 5).
//!
//! These are the *computable* slices of the five plan metrics (evidence
//! precision, merge quality, summary faithfulness, correction rate, utility):
//! everything that can be scored without a human label or an LLM judge. They are
//! pure functions so they can back a regression suite (the `tests` module below)
//! and, later, a reporting endpoint. The two metrics that genuinely need outside
//! signal — semantic faithfulness beyond citation coverage, and "reviews
//! accepted with light edits" (utility) — are intentionally left to the LLM
//! verifier (`super::verify_review`) and to future acceptance telemetry.

use super::{
    compact_evidence, is_sensitive, resolve_entities, review_citation_coverage, EntityRecord,
    EvidenceRecord, EvidenceStatus, EVIDENCE_SALIENCE_THRESHOLD,
};

/// Salience-gate precision over labeled cases `(importance, should_keep)`: of the
/// records the gate keeps, the fraction that should have been kept. Empty/no-keep
/// inputs score 1.0.
pub fn salience_precision(cases: &[(f64, bool)]) -> f64 {
    let mut kept = 0usize;
    let mut correct = 0usize;
    for (importance, should_keep) in cases {
        if *importance >= EVIDENCE_SALIENCE_THRESHOLD {
            kept += 1;
            if *should_keep {
                correct += 1;
            }
        }
    }
    if kept == 0 {
        1.0
    } else {
        correct as f64 / kept as f64
    }
}

/// Merge quality, conservative direction: the number of distinct active anchors
/// the resolver produces for a set of raw keys. Exact (case-insensitive)
/// key/alias matches merge; everything else stays separate. Use against a
/// fixture whose expected distinct count is known.
pub fn merge_distinct_count(keys: &[&str]) -> usize {
    let mut stored: Vec<EntityRecord> = Vec::new();
    let candidates: Vec<EntityRecord> = keys
        .iter()
        .filter_map(|k| EntityRecord::from_key(k, &[], &[], "2026-06-13T00:00:00Z"))
        .collect();
    resolve_entities(&mut stored, candidates);
    stored.len()
}

/// Summary faithfulness, deterministic slice: claim→evidence traceability =
/// the fraction of claim bullets that cite an admissible evidence id.
pub fn review_traceability(review_md: &str, input_ids: &[String]) -> f64 {
    review_citation_coverage(review_md, input_ids).0
}

/// Correction rate: the fraction of persisted records a user has corrected
/// (suppressed/deleted/re-faceted). A high rate is a signal the distiller or
/// facet proposer needs tuning.
pub fn correction_rate(records: &[EvidenceRecord]) -> f64 {
    if records.is_empty() {
        return 0.0;
    }
    let corrected = records.iter().filter(|r| r.is_user_corrected()).count();
    corrected as f64 / records.len() as f64
}

/// Merge-quality regression: the fraction of `(raw_keys, expected_distinct)`
/// fixture cases the resolver gets right — i.e. produces exactly the expected
/// number of distinct anchors, neither over- nor under-collapsing. `1.0` = all
/// correct. Seed it with known-distinct pairs (the failure mode that matters:
/// two different people/projects wrongly merged into one).
pub fn merge_quality_regression(cases: &[(&[&str], usize)]) -> f64 {
    if cases.is_empty() {
        return 1.0;
    }
    let correct = cases
        .iter()
        .filter(|(keys, expected)| merge_distinct_count(keys) == *expected)
        .count();
    correct as f64 / cases.len() as f64
}

/// A scope-level, deterministic health report over a collection of evidence —
/// the computable subset of the plan's quality metrics, assembled so it can back
/// a runnable report (the `evidence-eval` CLI) rather than only unit fixtures.
/// Records should be the RAW lanes (native ∪ user-owned, NOT suppression-filtered)
/// so `sensitive`/`corrected` reflect reality. LLM-graded evidence-vs-source
/// precision is intentionally not here — it needs source content + a judge model
/// (tracked as the remaining eval-depth follow-up).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EvidenceQualityReport {
    pub total: usize,
    /// Records eligible for synthesis (`status == Active`).
    pub live: usize,
    /// Records a human has touched (suppressed / deleted / user-faceted).
    pub corrected: usize,
    pub correction_rate: f64,
    /// Affirmatively-sensitive records (suppressed from default consumers).
    pub sensitive: usize,
    pub sensitive_fraction: f64,
    /// Records carrying at least one entity anchor.
    pub anchored: usize,
    pub anchored_fraction: f64,
    /// Distinct entity keys referenced across all records (fragmentation proxy).
    pub distinct_entities: usize,
    /// Records carrying at least one facet (domain classification coverage).
    pub faceted: usize,
    pub faceted_fraction: f64,
    /// Records that would still be eliminated by another compaction pass — should
    /// stay at/near 0 since compaction runs on every append; a non-zero value
    /// flags un-compacted duplicates (e.g. legacy data written before WS1).
    pub compaction_headroom: usize,
}

/// Compute the deterministic scope-level evidence health report. Pure.
pub fn evidence_quality_report(records: &[EvidenceRecord]) -> EvidenceQualityReport {
    let total = records.len();
    if total == 0 {
        return EvidenceQualityReport::default();
    }
    let live = records
        .iter()
        .filter(|r| r.status == EvidenceStatus::Active)
        .count();
    let corrected = records.iter().filter(|r| r.is_user_corrected()).count();
    let sensitive = records
        .iter()
        .filter(|r| is_sensitive(&r.sensitivity))
        .count();
    let anchored = records.iter().filter(|r| !r.entity_keys.is_empty()).count();
    let faceted = records.iter().filter(|r| !r.facets.is_empty()).count();
    let mut keys: Vec<&str> = records
        .iter()
        .flat_map(|r| r.entity_keys.iter().map(String::as_str))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let compaction_headroom = compact_evidence(records.to_vec()).merged;
    let frac = |n: usize| n as f64 / total as f64;
    EvidenceQualityReport {
        total,
        live,
        corrected,
        correction_rate: frac(corrected),
        sensitive,
        sensitive_fraction: frac(sensitive),
        anchored,
        anchored_fraction: frac(anchored),
        distinct_entities: keys.len(),
        faceted,
        faceted_fraction: frac(faceted),
        compaction_headroom,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::evidence::EvidenceStatus;

    fn ev(id: &str, importance: f64, suppressed: bool) -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: id.to_string(),
            summary: "s".to_string(),
            evidence_kind: "activity".to_string(),
            observed_actions: vec![],
            entity_keys: vec![],
            people_keys: vec![],
            artifact_refs: vec![],
            source_refs: vec![],
            facets: vec![],
            importance,
            confidence: 0.5,
            sensitivity: "unknown".to_string(),
            first_seen_at: "2026-06-13T00:00:00Z".to_string(),
            last_seen_at: "2026-06-13T00:00:00Z".to_string(),
            status: if suppressed {
                EvidenceStatus::Suppressed
            } else {
                EvidenceStatus::Active
            },
            last_corrected_at: None,
            producer: crate::magician_v2::evidence::default_producer(),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn salience_precision_scores_kept_records() {
        // kept: 0.5 (correct) + 0.45 (incorrect) → 1/2. The 0.3 case is below
        // the 0.4 gate and is not kept, so it doesn't count against precision.
        let cases = [(0.5, true), (0.3, false), (0.45, false)];
        assert!((salience_precision(&cases) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn merge_dedups_exact_keys_case_insensitively() {
        // project:a (x2, mixed case) collapse to one; project:b is separate.
        let n = merge_distinct_count(&["project:a", "project:a", "PROJECT:A", "project:b"]);
        assert_eq!(n, 2);
    }

    #[test]
    fn traceability_counts_only_admissible_citations() {
        let review = "- shipped x [evd:ep_1]\n- claimed y [evd:nope]\n- did z";
        let ids = vec!["evd:ep_1".to_string(), "evd:ep_2".to_string()];
        // 3 bullets, only the first cites an admissible id → 1/3.
        assert!((review_traceability(review, &ids) - (1.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn correction_rate_counts_user_touched() {
        let records = [ev("evd:1", 0.6, false), ev("evd:2", 0.6, true)];
        assert!((correction_rate(&records) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn merge_quality_regression_scores_distinct_expectations() {
        // case 1 correct: a/A collapse, b separate → expect 2, get 2.
        // case 2 wrong: x and y are distinct → expect 1, get 2.
        let cases: &[(&[&str], usize)] = &[
            (&["project:a", "PROJECT:A", "project:b"], 2),
            (&["person:x", "person:y"], 1),
        ];
        assert!((merge_quality_regression(cases) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn quality_report_counts_health_dimensions() {
        let mut anchored = ev("evd:1", 0.6, false);
        anchored.entity_keys = vec!["pr:1".to_string()];
        anchored.facets = vec![crate::magician_v2::evidence::Facet {
            label: "work".into(),
            confidence: 0.6,
            assigned_by: "llm".into(),
        }];
        let mut sensitive = ev("evd:2", 0.6, false);
        sensitive.sensitivity = "financial".to_string();
        let suppressed = ev("evd:3", 0.6, true); // user-corrected

        let report = evidence_quality_report(&[anchored, sensitive, suppressed]);
        assert_eq!(report.total, 3);
        assert_eq!(report.live, 2, "suppressed record is not live");
        assert_eq!(report.corrected, 1);
        assert_eq!(report.sensitive, 1);
        assert_eq!(report.anchored, 1);
        assert_eq!(report.distinct_entities, 1);
        assert_eq!(report.faceted, 1);
        assert_eq!(
            report.compaction_headroom, 0,
            "distinct records — nothing to merge"
        );
    }
}
