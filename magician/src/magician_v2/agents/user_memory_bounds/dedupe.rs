//! Dry run: user-memory records that say one thing in different words.
//!
//! The write-time fold ([`bounds::plan`]) catches restatements it can prove
//! cheaply: the same key however it is spelled, exact duplicates. It cannot
//! see two producers phrasing one preference differently ("return only the
//! draft text" against "draft text only, no labels"). This finds those with
//! the local embedding model: records in one section with the same
//! applicability are grouped when every pair in the group is at least
//! `threshold` similar. Closeness does not chain: A near B and B near C does
//! not put A with C, so a group never spans claims that drifted apart.
//! A record that is the memory of one observed event (keyed by its evidence
//! item, such as one browsing day) never groups with another event's record:
//! two days of similar meetings are two events, not one claim.
//!
//! Reads only. It prints what an owner-approved merge would do: which copy
//! survives (the owner's strongest, then the newest — the fold's own order)
//! and which leave for the archive. Groups holding an owner record are
//! marked: merging those needs the owner's word. It also counts what the
//! write-time fold itself will do on the next write, so nothing it archives
//! comes as a surprise.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use super::settings::MagicianMemoryBoundsSettings;
use crate::magician_v2::agents::memory_lifecycle::bounds::{
    self, duplicate_rank, protected_flags, scope_of,
};
use crate::magician_v2::agents::memory_lifecycle::{record_id, retired, text};
use crate::magician_v2::agents::storage::AgentStorage;

/// Sections compared by default: the agent-learned ones that grow.
pub const DEFAULT_SECTIONS: [&str; 5] = [
    "preferences",
    "workflows",
    "organization",
    "knowledge",
    "skills",
];
/// Cosine similarity at or above which two records are called one claim.
pub const DEFAULT_THRESHOLD: f32 = 0.9;
/// Characters of a record embedded (key plus wording).
const EMBED_CHARS: usize = 1_200;
/// Characters of a record shown in the report.
const PREVIEW_CHARS: usize = 240;
const EMBED_BATCH: usize = 64;

#[derive(Debug, Clone, Serialize)]
pub struct DedupeReport {
    pub threshold: f32,
    pub sections: Vec<SectionDuplicates>,
    /// What the write-time fold archives on the next write, by section and
    /// rule (`restated_by_producer`, `merged_duplicate`).
    pub write_time_fold: BTreeMap<String, BTreeMap<String, usize>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionDuplicates {
    pub section: String,
    pub records_compared: usize,
    pub groups: Vec<DuplicateGroup>,
    /// Records an approved merge of every group would archive.
    pub would_archive: usize,
    /// Of those, records in groups holding an owner record.
    pub would_archive_needing_owner: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    /// Holds an owner record (stated, confirmed, asked for, reviewed, or in
    /// an open question): merging it needs the owner's word.
    pub needs_owner: bool,
    /// Lowest similarity between any two members (never below the threshold).
    pub min_similarity: f32,
    pub survivor: GroupMember,
    pub others: Vec<GroupMember>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupMember {
    pub index: usize,
    pub record_id: String,
    pub key: Option<String>,
    pub owner_record: bool,
    pub similarity_to_survivor: f32,
    pub preview: String,
}

/// The local embedding model, on the background lane so a dry run never
/// delays a live retrieval.
pub async fn local_embed(texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
    magician_vector_index::vector_toolkit::OllamaEmbedder::from_env()
        .embed_documents_background(&texts)
        .await
}

pub(crate) fn embed_text(item: &Value) -> String {
    let key = item
        .get("key")
        .and_then(Value::as_str)
        .map(|key| key.replace(['_', '-', '.'], " "))
        .unwrap_or_default();
    let mut text = format!("{key}: {}", text(item));
    if text.chars().count() > EMBED_CHARS {
        text = text.chars().take(EMBED_CHARS).collect();
    }
    text
}

fn preview(item: &Value) -> String {
    let text = text(item);
    let mut out: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// The observed event a record is the memory of, when its producer keyed it
/// by one evidence item (the ambient bridge upserts one `user.knowledge`
/// record per host and day under the evidence id).
fn observation_of(item: &Value) -> Option<&str> {
    item.get("evidence_refs")?
        .as_array()?
        .iter()
        .filter(|evidence| evidence.get("kind").and_then(Value::as_str) == Some("work_evidence"))
        .find_map(|evidence| evidence.get("id").and_then(Value::as_str))
}

/// Groups of positions in `vectors` with the same `scopes` entry in which
/// every pair is at least `threshold` similar. Closest pairs are taken first;
/// a record joins a group only when it is close to every member, and two
/// groups never merge, so closeness does not chain.
fn similar_groups(vectors: &[Vec<f32>], scopes: &[String], threshold: f32) -> Vec<Vec<usize>> {
    let mut close: Vec<(f32, usize, usize)> = Vec::new();
    for i in 0..vectors.len() {
        for j in i + 1..vectors.len() {
            if scopes[i] == scopes[j] {
                let similarity = cosine(&vectors[i], &vectors[j]);
                if similarity >= threshold {
                    close.push((similarity, i, j));
                }
            }
        }
    }
    let is_close: HashSet<(usize, usize)> = close.iter().map(|&(_, i, j)| (i, j)).collect();
    let near = |a: usize, b: usize| is_close.contains(&(a.min(b), a.max(b)));
    close.sort_by(|a, b| b.0.total_cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2))));
    let mut group_of: Vec<Option<usize>> = vec![None; vectors.len()];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (_, i, j) in close {
        match (group_of[i], group_of[j]) {
            (None, None) => {
                group_of[i] = Some(groups.len());
                group_of[j] = Some(groups.len());
                groups.push(vec![i, j]);
            },
            (Some(group), None) | (None, Some(group)) => {
                let joining = if group_of[i].is_none() { i } else { j };
                if groups[group].iter().all(|&member| near(member, joining)) {
                    group_of[joining] = Some(group);
                    groups[group].push(joining);
                }
            },
            (Some(_), Some(_)) => {},
        }
    }
    for group in &mut groups {
        group.sort_unstable();
    }
    groups
}

/// Build the report. `embed` turns texts into vectors (the local embedding
/// model in production, a stub in tests).
pub async fn dry_run<F, Fut>(
    storage: &AgentStorage,
    settings: &MagicianMemoryBoundsSettings,
    sections: &[String],
    threshold: f32,
    now: DateTime<Utc>,
    embed: F,
) -> anyhow::Result<DedupeReport>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = anyhow::Result<Vec<Vec<f32>>>>,
{
    let document = storage.load_user_knowledge().await?;
    report(&document, settings, sections, threshold, now, embed).await
}

pub async fn report<F, Fut>(
    document: &Value,
    settings: &MagicianMemoryBoundsSettings,
    sections: &[String],
    threshold: f32,
    now: DateTime<Utc>,
    embed: F,
) -> anyhow::Result<DedupeReport>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = anyhow::Result<Vec<Vec<f32>>>>,
{
    let mut write_time_fold: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for ((section, reason), count) in bounds::plan(document, settings, None, now).counts() {
        if matches!(reason, "restated_by_producer" | "merged_duplicate") {
            write_time_fold
                .entry(section)
                .or_default()
                .insert(reason.to_string(), count);
        }
    }

    let mut out = Vec::new();
    for section in sections {
        let Some(items) = document.get(section).and_then(Value::as_array) else {
            continue;
        };
        let owner = protected_flags(document, section);
        let live: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_object() && !retired(item))
            .map(|(index, _)| index)
            .collect();
        let mut vectors = Vec::with_capacity(live.len());
        for batch in live.chunks(EMBED_BATCH) {
            let texts = batch
                .iter()
                .map(|&index| embed_text(&items[index]))
                .collect();
            let embedded = embed(texts).await?;
            anyhow::ensure!(
                embedded.len() == batch.len(),
                "the embedding model returned {} vectors for {} texts",
                embedded.len(),
                batch.len()
            );
            vectors.extend(embedded);
        }
        let scopes: Vec<String> = live
            .iter()
            .map(|&index| match observation_of(&items[index]) {
                Some(event) => format!("{}|observation:{event}", scope_of(&items[index])),
                None => scope_of(&items[index]).to_string(),
            })
            .collect();

        let mut groups = Vec::new();
        for positions in similar_groups(&vectors, &scopes, threshold) {
            let survivor_at = *positions
                .iter()
                .max_by_key(|&&at| (duplicate_rank(&items[live[at]]), live[at]))
                .expect("group has members");
            let member = |at: usize| {
                let index = live[at];
                GroupMember {
                    index,
                    record_id: record_id(&items[index]),
                    key: items[index]
                        .get("key")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    owner_record: owner.get(index).copied().unwrap_or(true),
                    similarity_to_survivor: cosine(&vectors[at], &vectors[survivor_at]),
                    preview: preview(&items[index]),
                }
            };
            let survivor = member(survivor_at);
            let others: Vec<GroupMember> = positions
                .iter()
                .filter(|&&at| at != survivor_at)
                .map(|&at| member(at))
                .collect();
            let needs_owner = survivor.owner_record || others.iter().any(|m| m.owner_record);
            let min_similarity = positions
                .iter()
                .enumerate()
                .flat_map(|(n, &a)| positions[n + 1..].iter().map(move |&b| (a, b)))
                .map(|(a, b)| cosine(&vectors[a], &vectors[b]))
                .fold(1.0f32, f32::min);
            groups.push(DuplicateGroup {
                needs_owner,
                min_similarity,
                survivor,
                others,
            });
        }
        groups.sort_by(|a, b| b.others.len().cmp(&a.others.len()));
        let would_archive = groups.iter().map(|g| g.others.len()).sum();
        let would_archive_needing_owner = groups
            .iter()
            .filter(|g| g.needs_owner)
            .map(|g| g.others.len())
            .sum();
        out.push(SectionDuplicates {
            section: section.clone(),
            records_compared: live.len(),
            groups,
            would_archive,
            would_archive_needing_owner,
        });
    }
    Ok(DedupeReport {
        threshold,
        sections: out,
        write_time_fold,
    })
}

/// Plain-text rendering: totals, then every group.
pub fn render(report: &DedupeReport) -> String {
    let mut out = String::new();
    out.push_str("memory-dedupe-user: dry run (nothing written)\n");
    out.push_str("\nwrite-time fold (happens automatically on the next write in enforce mode):\n");
    if report.write_time_fold.is_empty() {
        out.push_str("  nothing\n");
    }
    for (section, reasons) in &report.write_time_fold {
        for (reason, count) in reasons {
            out.push_str(&format!("  {section}: {count} ({reason})\n"));
        }
    }
    out.push_str(&format!(
        "\nsame meaning, different words (similarity >= {:.2}):\n",
        report.threshold
    ));
    for section in &report.sections {
        out.push_str(&format!(
            "  {}: {} compared, {} groups, {} would be archived ({} need the owner's OK)\n",
            section.section,
            section.records_compared,
            section.groups.len(),
            section.would_archive,
            section.would_archive_needing_owner
        ));
    }
    for section in &report.sections {
        for (n, group) in section.groups.iter().enumerate() {
            out.push_str(&format!(
                "\n[{} #{}] {} records, min similarity {:.3}{}\n",
                section.section,
                n + 1,
                group.others.len() + 1,
                group.min_similarity,
                if group.needs_owner {
                    ", needs owner OK"
                } else {
                    ""
                }
            ));
            let line = |m: &GroupMember, role: &str| {
                format!(
                    "  {role} {:.3} {}{} | {}\n",
                    m.similarity_to_survivor,
                    m.key.as_deref().unwrap_or("-"),
                    if m.owner_record { " [owner]" } else { "" },
                    m.preview.replace('\n', " ")
                )
            };
            out.push_str(&line(&group.survivor, "KEEP"));
            for other in &group.others {
                out.push_str(&line(other, "  ->"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A stub model: texts mentioning "draft" point one way, others another.
    async fn stub(texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                if t.contains("draft") {
                    vec![1.0, 0.02]
                } else if t.contains("metric") {
                    vec![0.0, 1.0]
                } else {
                    vec![0.7, -0.7]
                }
            })
            .collect())
    }

    #[tokio::test]
    async fn paraphrases_group_owner_records_are_flagged_and_nothing_is_written() {
        let document = json!({"preferences": [
            {"key":"response_format_draft_only","value":"Respond with only the draft text",
             "source_type":"inferred","updated_at":"2026-09-01T00:00:00Z"},
            {"key":"contextual_writing_output_format","value":"draft text only, no labels",
             "source_type":"inferred","updated_at":"2026-09-03T00:00:00Z"},
            {"key":"units","value":"Use metric units","source_type":"explicit_user_statement",
             "updated_at":"2026-09-02T00:00:00Z"},
            {"key":"units_again","value":"metric please","source_type":"inferred",
             "updated_at":"2026-09-04T00:00:00Z"},
            {"key":"tone","value":"Warm and brief","source_type":"inferred",
             "updated_at":"2026-09-05T00:00:00Z"}
        ]});
        let before = document.clone();
        let report = report(
            &document,
            &MagicianMemoryBoundsSettings::default(),
            &["preferences".to_string()],
            DEFAULT_THRESHOLD,
            now(),
            stub,
        )
        .await
        .unwrap();
        assert_eq!(document, before);
        let section = &report.sections[0];
        assert_eq!(section.records_compared, 5);
        assert_eq!(section.groups.len(), 2);
        assert_eq!(section.would_archive, 2);
        assert_eq!(section.would_archive_needing_owner, 1);
        let units = section
            .groups
            .iter()
            .find(|g| g.needs_owner)
            .expect("owner group");
        // The owner's own record survives, whatever its age.
        assert_eq!(units.survivor.key.as_deref(), Some("units"));
        let drafts = section.groups.iter().find(|g| !g.needs_owner).unwrap();
        // Agent-made: the newest wording survives.
        assert_eq!(
            drafts.survivor.key.as_deref(),
            Some("contextual_writing_output_format")
        );
        assert!(render(&report).contains("needs owner OK"));
    }

    #[test]
    fn closeness_does_not_chain() {
        // A~B 0.94 and B~C 0.91, but A~C only 0.71: A and C never share a group.
        let at = |degrees: f32| vec![degrees.to_radians().cos(), degrees.to_radians().sin()];
        let vectors = vec![at(0.0), at(20.0), at(45.0)];
        let scopes = vec![String::new(); 3];
        assert_eq!(similar_groups(&vectors, &scopes, 0.9), vec![vec![0, 1]]);
        // A fourth record close to both A and B joins their group; C stays out.
        let vectors = vec![at(0.0), at(20.0), at(45.0), at(10.0)];
        let scopes = vec![String::new(); 4];
        assert_eq!(similar_groups(&vectors, &scopes, 0.9), vec![vec![0, 1, 3]]);
    }

    #[tokio::test]
    async fn two_observed_events_are_never_one_claim() {
        let day = |id: &str, value: &str| {
            json!({"key": id.replace([':', '.'], "_"), "value": value,
                   "source_type": "learning_candidate", "updated_at": "2026-07-11T00:00:00Z",
                   "evidence_refs": [{"kind": "work_evidence", "id": id}]})
        };
        let document = json!({"knowledge": [
            day("evd:amb:meet.google.com:2026-07-10", "draft review meetings"),
            day("evd:amb:meet.google.com:2026-07-11", "draft review meetings again"),
            {"key": "a", "value": "draft one", "source_type": "inferred"},
            {"key": "b", "value": "draft two", "source_type": "inferred"}
        ]});
        let report = report(
            &document,
            &MagicianMemoryBoundsSettings::default(),
            &["knowledge".to_string()],
            DEFAULT_THRESHOLD,
            now(),
            stub,
        )
        .await
        .unwrap();
        let groups = &report.sections[0].groups;
        assert_eq!(groups.len(), 1, "only the two plain records group");
        let mut keys = vec![
            groups[0].survivor.key.clone(),
            groups[0].others[0].key.clone(),
        ];
        keys.sort();
        assert_eq!(keys, vec![Some("a".to_string()), Some("b".to_string())]);
    }

    #[test]
    fn different_applicability_never_groups() {
        let vectors = vec![vec![1.0, 0.0], vec![1.0, 0.0]];
        let scopes = vec!["a".to_string(), "b".to_string()];
        assert!(similar_groups(&vectors, &scopes, 0.9).is_empty());
    }
}
