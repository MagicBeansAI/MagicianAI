//! Impact dashboard assembly (Phase 3 — career retrieval + surfaces).
//!
//! Deterministic roll-up of an agent's accrued evidence + entities into a
//! dashboard payload: headline counts, coverage by facet, entity-type mix, top
//! entities, weekly activity, recent evidence, and visibility gaps. Serves both
//! the `/evidence/dashboard` read endpoint (JSON for the UI) and the
//! `/evidence/dashboard/publish` path (rendered to Markdown and pushed through
//! the artifact-driven surface machinery onto `/briefing`).

use std::collections::{HashMap, HashSet};

use chrono::Datelike;
use serde::Serialize;

use super::{EntityRecord, EvidenceRecord, EvidenceStatus};

#[derive(Debug, Clone, Serialize)]
pub struct LabelCount {
    pub label: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopEntity {
    pub name: String,
    pub entity_type: String,
    pub sources: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentItem {
    pub summary: String,
    pub kind: String,
    pub facets: String,
    pub when: String,
}

/// The full dashboard payload. Scalar counts render as a metric grid; the
/// arrays render as tables / lists.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardData {
    pub agent: String,
    pub facet: String,
    pub window_days: Option<i64>,
    pub generated_at: String,
    pub total_evidence: usize,
    pub active_entities: usize,
    pub facets_tracked: usize,
    pub reviews_generated: usize,
    pub facet_coverage: Vec<LabelCount>,
    pub entity_types: Vec<LabelCount>,
    pub top_entities: Vec<TopEntity>,
    pub weekly_activity: Vec<LabelCount>,
    pub recent_evidence: Vec<RecentItem>,
    pub visibility_gaps: Vec<String>,
}

fn counts_desc(map: HashMap<String, usize>) -> Vec<LabelCount> {
    let mut out: Vec<LabelCount> = map
        .into_iter()
        .map(|(label, count)| LabelCount { label, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.label.cmp(&b.label)));
    out
}

#[derive(Debug, Clone)]
struct EntityRollup {
    name: String,
    entity_type: String,
    source_refs: HashSet<String>,
}

fn entity_rollup_key(entity_type: &str, canonical_name: &str) -> String {
    format!(
        "{}:{}",
        entity_type.trim().to_ascii_lowercase(),
        canonical_name.trim().to_ascii_lowercase()
    )
}

fn roll_up_entities(entities: &[&EntityRecord]) -> Vec<EntityRollup> {
    let mut by_display: HashMap<String, EntityRollup> = HashMap::new();
    for entity in entities {
        let key = entity_rollup_key(&entity.entity_type, &entity.canonical_name);
        let entry = by_display.entry(key).or_insert_with(|| EntityRollup {
            name: entity.canonical_name.clone(),
            entity_type: entity.entity_type.clone(),
            source_refs: HashSet::new(),
        });
        for source_ref in &entity.source_refs {
            entry.source_refs.insert(source_ref.clone());
        }
    }

    by_display.into_values().collect()
}

fn iso_week_key(rfc3339: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|dt| {
            let iso = dt.iso_week();
            format!("{}-W{:02}", iso.year(), iso.week())
        })
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Build the dashboard from active evidence + entities (callers pre-filter by
/// facet/window). `reviews_generated` is supplied by the caller (it scans the
/// durable review namespace). Pure + deterministic.
pub fn build_dashboard(
    agent: &str,
    facet: &str,
    window_days: Option<i64>,
    generated_at: String,
    records: &[EvidenceRecord],
    entities: &[EntityRecord],
    reviews_generated: usize,
) -> DashboardData {
    let active: Vec<&EvidenceRecord> = records
        .iter()
        .filter(|r| r.status == EvidenceStatus::Active)
        .collect();
    let active_entities: Vec<&EntityRecord> = entities
        .iter()
        .filter(|e| e.status == EvidenceStatus::Active && e.merged_into.is_none())
        .collect();

    let dashboard_entities = roll_up_entities(&active_entities);

    let mut facet_map: HashMap<String, usize> = HashMap::new();
    for r in &active {
        for f in &r.facets {
            *facet_map.entry(f.label.clone()).or_insert(0) += 1;
        }
    }
    let facets_tracked = facet_map.len();
    let facet_coverage = counts_desc(facet_map);

    let mut type_map: HashMap<String, usize> = HashMap::new();
    for e in &dashboard_entities {
        *type_map.entry(e.entity_type.clone()).or_insert(0) += 1;
    }
    let entity_types = counts_desc(type_map);

    let mut week_map: HashMap<String, usize> = HashMap::new();
    for r in &active {
        if let Some(week) = iso_week_key(&r.last_seen_at) {
            *week_map.entry(week).or_insert(0) += 1;
        }
    }
    let mut weekly_activity = counts_desc(week_map);
    // Chronological for the activity table (label asc), not by count.
    weekly_activity.sort_by(|a, b| a.label.cmp(&b.label));

    let mut by_sources = dashboard_entities.clone();
    by_sources.sort_by(|a, b| {
        b.source_refs
            .len()
            .cmp(&a.source_refs.len())
            .then(a.name.cmp(&b.name))
    });
    let top_entities = by_sources
        .iter()
        .take(10)
        .map(|e| TopEntity {
            name: e.name.clone(),
            entity_type: e.entity_type.clone(),
            sources: e.source_refs.len(),
        })
        .collect();

    let mut recent: Vec<&EvidenceRecord> = active.clone();
    recent.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
    let recent_evidence = recent
        .iter()
        .take(10)
        .map(|r| RecentItem {
            summary: truncate(&r.summary, 140),
            kind: r.evidence_kind.clone(),
            facets: if r.facets.is_empty() {
                "-".to_string()
            } else {
                r.facets
                    .iter()
                    .map(|f| f.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            when: r.last_seen_at.chars().take(10).collect(),
        })
        .collect();

    // Visibility gaps — honest, computed signals the user can act on.
    let mut visibility_gaps = Vec::new();
    if active.is_empty() {
        visibility_gaps.push("No active evidence in scope yet.".to_string());
    }
    if reviews_generated == 0 && !active.is_empty() {
        visibility_gaps.push("No impact review generated yet for this scope.".to_string());
    }
    for fc in &facet_coverage {
        if fc.count == 1 {
            visibility_gaps.push(format!(
                "Facet '{}' has only 1 supporting record — thin coverage.",
                fc.label
            ));
        }
    }
    let single_source = dashboard_entities
        .iter()
        .filter(|e| e.source_refs.len() <= 1)
        .count();
    if single_source > 0 {
        visibility_gaps.push(format!(
            "{single_source} entity/entities seen in only one episode — low corroboration."
        ));
    }

    DashboardData {
        agent: agent.to_string(),
        facet: facet.to_string(),
        window_days,
        generated_at,
        total_evidence: active.len(),
        active_entities: dashboard_entities.len(),
        facets_tracked,
        reviews_generated,
        facet_coverage,
        entity_types,
        top_entities,
        weekly_activity,
        recent_evidence,
        visibility_gaps,
    }
}

/// Render a dashboard to Markdown for the published surface (the artifact-driven
/// surface materializer compiles this into the `/briefing` scroll view).
pub fn render_dashboard_markdown(d: &DashboardData) -> String {
    let mut s = String::new();
    let window = match d.window_days {
        Some(days) => format!("last {days} day(s)"),
        None => "all time".to_string(),
    };
    s.push_str(&format!(
        "# Impact dashboard — {} · {}\n\n",
        d.facet, window
    ));
    s.push_str(&format!(
        "> Generated {} · agent `{}`\n\n",
        d.generated_at, d.agent
    ));

    s.push_str("## At a glance\n\n");
    s.push_str(&format!("- **{}** evidence records\n", d.total_evidence));
    s.push_str(&format!("- **{}** active entities\n", d.active_entities));
    s.push_str(&format!("- **{}** facets tracked\n", d.facets_tracked));
    s.push_str(&format!(
        "- **{}** reviews generated\n\n",
        d.reviews_generated
    ));

    if !d.facet_coverage.is_empty() {
        s.push_str("## Coverage by facet\n\n| Facet | Records |\n|---|---|\n");
        for fc in &d.facet_coverage {
            s.push_str(&format!("| {} | {} |\n", fc.label, fc.count));
        }
        s.push('\n');
    }

    if !d.top_entities.is_empty() {
        s.push_str("## Top entities\n\n| Entity | Type | Sources |\n|---|---|---|\n");
        for e in &d.top_entities {
            s.push_str(&format!(
                "| {} | {} | {} |\n",
                e.name, e.entity_type, e.sources
            ));
        }
        s.push('\n');
    }

    if !d.weekly_activity.is_empty() {
        s.push_str("## Activity by week\n\n| Week | Records |\n|---|---|\n");
        for w in &d.weekly_activity {
            s.push_str(&format!("| {} | {} |\n", w.label, w.count));
        }
        s.push('\n');
    }

    if !d.recent_evidence.is_empty() {
        s.push_str("## Recent evidence\n\n");
        for r in &d.recent_evidence {
            s.push_str(&format!(
                "- _[{}]_ {} — `{}` · {}\n",
                r.kind, r.summary, r.facets, r.when
            ));
        }
        s.push('\n');
    }

    if !d.visibility_gaps.is_empty() {
        s.push_str("## Visibility gaps\n\n");
        for g in &d.visibility_gaps {
            s.push_str(&format!("- {g}\n"));
        }
        s.push('\n');
    }

    s
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn entity(entity_key: &str, canonical_name: &str, sources: &[&str]) -> EntityRecord {
        EntityRecord {
            entity_key: entity_key.to_string(),
            entity_type: "person".to_string(),
            canonical_name: canonical_name.to_string(),
            aliases: Vec::new(),
            source_refs: sources.iter().map(|source| source.to_string()).collect(),
            facets: Vec::new(),
            confidence: 0.7,
            first_seen_at: "2026-06-20T00:00:00Z".to_string(),
            last_seen_at: "2026-06-25T00:00:00Z".to_string(),
            status: EvidenceStatus::Active,
            merged_into: None,
            user_curated: false,
            producer: "task_episode".to_string(),
        }
    }

    #[test]
    fn dashboard_rolls_up_duplicate_display_entities() {
        let entities = vec![
            entity(
                "person:ruchika-kar",
                "Priya Rao",
                &["episode:a", "episode:b"],
            ),
            entity(
                "person:ruchika kar",
                "Priya Rao",
                &["episode:b", "episode:c"],
            ),
        ];

        let dashboard = build_dashboard(
            "personal-assistant",
            "work",
            Some(7),
            "2026-07-04T00:00:00Z".to_string(),
            &[],
            &entities,
            0,
        );

        assert_eq!(dashboard.active_entities, 1);
        assert_eq!(dashboard.entity_types.len(), 1);
        assert_eq!(dashboard.entity_types[0].count, 1);
        assert_eq!(dashboard.top_entities.len(), 1);
        assert_eq!(dashboard.top_entities[0].name, "Priya Rao");
        assert_eq!(dashboard.top_entities[0].sources, 3);
    }
}
