//! Layer-3 derived views over the evidence base (WEG generic substrate).
//!
//! The design's Layer-3 is "the graph lives as compact entity/relation **views
//! derived from evidence**, not a new graph product." Realized here as typed,
//! deterministic, **re-runnable Rust projections** over the same JSON evidence
//! that `dashboard.rs` reads — no durable edge store, so sparse relations stay a
//! *derived* co-occurrence view (honoring "defer durable structure"). A
//! DuckDB-SQL realization would need an evidence→Parquet mirror first; until a
//! consumer needs ad-hoc SQL, typed projections are the lighter, faithful form.
//!
//! Every view is **facet-parameterized** and skips affirmatively-sensitive
//! records, so the same query serves any domain (work, health, …) by facet.

use std::collections::HashMap;

use serde::Serialize;

use super::dashboard::LabelCount;
use super::{is_sensitive, EvidenceRecord, EvidenceStatus};

/// A sparse, derived co-occurrence relation: two entity keys cited together in
/// `weight` in-scope evidence records. NOT a durable edge — recomputed on demand.
#[derive(Debug, Clone, Serialize)]
pub struct CooccurrenceEdge {
    pub a: String,
    pub b: String,
    pub weight: usize,
}

/// The neighborhood of one entity: how much evidence touches it, and which other
/// entities / people / evidence-kinds co-occur with it.
#[derive(Debug, Clone, Serialize)]
pub struct EntityNeighborhood {
    pub entity_key: String,
    pub evidence_count: usize,
    pub co_entities: Vec<LabelCount>,
    pub people: Vec<LabelCount>,
    pub evidence_kinds: Vec<LabelCount>,
    pub recent_evidence_ids: Vec<String>,
}

/// In scope for a view: live (`Active`), not affirmatively-sensitive, and — when
/// a facet filter is given — carrying that facet (`"all"` / `None` = no filter).
fn in_scope(record: &EvidenceRecord, facet: Option<&str>) -> bool {
    if record.status != EvidenceStatus::Active || is_sensitive(&record.sensitivity) {
        return false;
    }
    match facet {
        None => true,
        Some(f) => record
            .facets
            .iter()
            .any(|x| x.label.eq_ignore_ascii_case(f)),
    }
}

fn counts_desc(map: HashMap<String, usize>) -> Vec<LabelCount> {
    let mut out: Vec<LabelCount> = map
        .into_iter()
        .map(|(label, count)| LabelCount { label, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.label.cmp(&b.label)));
    out
}

/// Sparse co-occurrence edges: unordered entity-key pairs co-cited in ≥
/// `min_weight` in-scope records, strongest first (deterministic ties by key).
pub fn cooccurrence_edges(
    records: &[EvidenceRecord],
    facet: Option<&str>,
    min_weight: usize,
) -> Vec<CooccurrenceEdge> {
    let min_weight = min_weight.max(1);
    let mut pairs: HashMap<(String, String), usize> = HashMap::new();
    for record in records.iter().filter(|r| in_scope(r, facet)) {
        let mut keys: Vec<&str> = record.entity_keys.iter().map(String::as_str).collect();
        keys.sort_unstable();
        keys.dedup();
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                *pairs
                    .entry((keys[i].to_string(), keys[j].to_string()))
                    .or_insert(0) += 1;
            }
        }
    }
    let mut edges: Vec<CooccurrenceEdge> = pairs
        .into_iter()
        .filter(|(_, weight)| *weight >= min_weight)
        .map(|((a, b), weight)| CooccurrenceEdge { a, b, weight })
        .collect();
    edges.sort_by(|x, y| {
        y.weight
            .cmp(&x.weight)
            .then(x.a.cmp(&y.a))
            .then(x.b.cmp(&y.b))
    });
    edges
}

/// Entity neighborhood: evidence touching `entity_key` + co-occurring entities /
/// people / kinds + recent evidence ids (newest first, capped at 10). Deterministic.
pub fn entity_neighborhood(
    records: &[EvidenceRecord],
    entity_key: &str,
    facet: Option<&str>,
) -> EntityNeighborhood {
    let mut co_entities: HashMap<String, usize> = HashMap::new();
    let mut people: HashMap<String, usize> = HashMap::new();
    let mut kinds: HashMap<String, usize> = HashMap::new();
    let mut touching: Vec<&EvidenceRecord> = Vec::new();

    for record in records.iter().filter(|r| in_scope(r, facet)) {
        if !record.entity_keys.iter().any(|k| k == entity_key) {
            continue;
        }
        touching.push(record);
        for key in &record.entity_keys {
            if key != entity_key {
                *co_entities.entry(key.clone()).or_insert(0) += 1;
            }
        }
        for person in &record.people_keys {
            *people.entry(person.clone()).or_insert(0) += 1;
        }
        *kinds.entry(record.evidence_kind.clone()).or_insert(0) += 1;
    }

    touching.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
    let recent_evidence_ids = touching
        .iter()
        .take(10)
        .map(|r| r.evidence_id.clone())
        .collect();

    EntityNeighborhood {
        entity_key: entity_key.to_string(),
        evidence_count: touching.len(),
        co_entities: counts_desc(co_entities),
        people: counts_desc(people),
        evidence_kinds: counts_desc(kinds),
        recent_evidence_ids,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::evidence::Facet;

    fn rec(id: &str, entities: &[&str], facet: &str, sensitivity: &str) -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: id.to_string(),
            summary: "s".into(),
            evidence_kind: "activity".into(),
            observed_actions: vec![],
            entity_keys: entities.iter().map(|s| s.to_string()).collect(),
            people_keys: vec![],
            artifact_refs: vec![],
            source_refs: vec![],
            facets: vec![Facet {
                label: facet.into(),
                confidence: 0.6,
                assigned_by: "llm".into(),
            }],
            importance: 0.6,
            confidence: 0.6,
            sensitivity: sensitivity.into(),
            first_seen_at: "2026-06-13T00:00:00Z".into(),
            last_seen_at: format!("2026-06-13T00:00:0{}Z", id.len() % 10),
            status: EvidenceStatus::Active,
            last_corrected_at: None,
            producer: "task_episode".into(),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn cooccurrence_weights_and_min_filter() {
        let records = vec![
            rec("1", &["a", "b"], "work", "work"),
            rec("2", &["a", "b", "c"], "work", "work"),
        ];
        let all = cooccurrence_edges(&records, None, 1);
        assert_eq!(all.len(), 3, "a-b, a-c, b-c");
        assert_eq!(all[0].a, "a");
        assert_eq!(all[0].b, "b");
        assert_eq!(all[0].weight, 2, "a-b cited in both records");

        let strong = cooccurrence_edges(&records, None, 2);
        assert_eq!(strong.len(), 1, "only a-b clears min_weight=2");
        assert_eq!(strong[0].weight, 2);
    }

    #[test]
    fn cooccurrence_excludes_sensitive_and_off_facet() {
        let records = vec![
            rec("1", &["a", "b"], "work", "work"),
            rec("2", &["a", "b"], "work", "financial"), // sensitive → excluded
            rec("3", &["a", "b"], "personal", "personal"), // off-facet for "work"
        ];
        let work = cooccurrence_edges(&records, Some("work"), 1);
        assert_eq!(work.len(), 1);
        assert_eq!(
            work[0].weight, 1,
            "only the non-sensitive work record counts"
        );
    }

    #[test]
    fn neighborhood_counts_co_entities_and_people() {
        let records = vec![
            rec("1", &["proj:x", "pr:1"], "work", "work"),
            rec("2", &["proj:x", "pr:2"], "work", "work"),
            rec("3", &["proj:y"], "work", "work"),
        ];
        let nb = entity_neighborhood(&records, "proj:x", None);
        assert_eq!(nb.evidence_count, 2);
        assert_eq!(nb.co_entities.len(), 2, "pr:1 + pr:2");
        assert!(nb.recent_evidence_ids.contains(&"1".to_string()));
    }
}
