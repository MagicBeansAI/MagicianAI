//! Read-only size report for the user-memory file
//! (`magician memory-user-report`). Counts, sizes and dates only: no record
//! contents are ever printed. Reads without taking the file lock (writes are
//! atomic renames, so an unlocked read is a whole document).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::archive;
use super::settings::MagicianMemoryBoundsSettings;
use crate::magician_v2::agents::memory_lifecycle::bounds::waiting_for_review;
use crate::magician_v2::agents::storage::AgentStorage;
use crate::magician_v2::agents::user_memory_lanes::{self, Lane};

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SectionReport {
    pub name: String,
    pub entries: usize,
    pub distinct_keys: usize,
    /// Compact JSON bytes.
    pub bytes: usize,
    /// Entries that are an older copy of a key written again later.
    pub older_copy_entries: usize,
    pub older_copy_bytes: usize,
    pub waiting_for_review: usize,
    /// Records from before the reviewer (neither review flag).
    pub pre_review_records: usize,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct LaneReport {
    pub name: String,
    pub rows: usize,
    pub file_bytes: u64,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ArchiveReport {
    pub files: usize,
    pub bytes: u64,
    pub months: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct UserMemoryReport {
    pub file_bytes: u64,
    pub compact_bytes: usize,
    pub hot_file_target_bytes: u64,
    pub over_target: bool,
    pub mode: String,
    pub sections: Vec<SectionReport>,
    pub waiting_for_review_total: usize,
    pub lanes: Vec<LaneReport>,
    pub archive: ArchiveReport,
}

fn compact_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn item_date(item: &Value) -> Option<String> {
    ["memory_saved_at", "updated_at", "day", "date"]
        .iter()
        .find_map(|field| item.get(*field).and_then(Value::as_str))
        .map(|raw| raw.chars().take(10).collect())
}

/// Report one array of records under `name`.
pub fn section_report(name: &str, items: &[Value]) -> SectionReport {
    let mut report = SectionReport {
        name: name.to_string(),
        entries: items.len(),
        bytes: compact_len(&Value::Array(items.to_vec())),
        ..Default::default()
    };
    // Newest copy per key wins; the rest are older copies.
    let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(key) = item.get("key").and_then(Value::as_str) {
            by_key
                .entry(key.trim().to_lowercase())
                .or_default()
                .push(index);
        }
        if waiting_for_review(item) {
            report.waiting_for_review += 1;
        }
        if item.is_object()
            && item.get("memory_review_needed").is_none()
            && item.get("memory_review_version").is_none()
        {
            report.pre_review_records += 1;
        }
        if let Some(date) = item_date(item) {
            if report.oldest.as_ref().is_none_or(|oldest| &date < oldest) {
                report.oldest = Some(date.clone());
            }
            if report.newest.as_ref().is_none_or(|newest| &date > newest) {
                report.newest = Some(date);
            }
        }
    }
    report.distinct_keys = by_key.len();
    for indices in by_key.values().filter(|indices| indices.len() > 1) {
        let newest = indices
            .iter()
            .max_by_key(|&&index| (item_date(&items[index]), index))
            .copied();
        for &index in indices.iter().filter(|&&index| Some(index) != newest) {
            report.older_copy_entries += 1;
            report.older_copy_bytes += compact_len(&items[index]);
        }
    }
    report
}

/// Report every list in a knowledge document: top-level arrays, arrays in the
/// nested `user` object (as `user.<name>`), and the promotions audit.
pub fn document_sections(document: &Value) -> Vec<SectionReport> {
    let mut sections = Vec::new();
    let Some(map) = document.as_object() else {
        return sections;
    };
    for (name, value) in map {
        match value {
            Value::Array(items) if !name.starts_with('_') => {
                sections.push(section_report(name, items));
            },
            Value::Object(nested) if name == "user" => {
                for (inner, value) in nested {
                    if let Some(items) = value.as_array() {
                        sections.push(section_report(&format!("user.{inner}"), items));
                    }
                }
            },
            _ => {},
        }
    }
    if let Some(items) = document
        .get("_meta")
        .and_then(|meta| meta.get("learning_promotions"))
        .and_then(Value::as_array)
    {
        let mut audit = section_report("_meta.learning_promotions", items);
        // An audit log is never reviewed.
        audit.waiting_for_review = 0;
        audit.pre_review_records = 0;
        sections.push(audit);
    }
    sections.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(&b.name)));
    sections
}

fn archive_report(user_root: &Path) -> ArchiveReport {
    let files = archive::month_files(user_root);
    let mut months = BTreeSet::new();
    let mut bytes = 0;
    for path in &files {
        bytes += std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            months.insert(name.chars().take(7).collect::<String>());
        }
    }
    ArchiveReport {
        files: files.len(),
        bytes,
        months: months.into_iter().collect(),
    }
}

/// Build the report for one scope's user memory.
pub async fn build(
    storage: &AgentStorage,
    settings: &MagicianMemoryBoundsSettings,
) -> Result<UserMemoryReport, crate::magician_v2::agents::storage::AgentStorageError> {
    let path = storage.user_knowledge_path();
    let file_bytes = tokio::fs::metadata(&path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let document = storage.load_user_knowledge().await?;
    let (compact_bytes, sections) =
        tokio::task::spawn_blocking(move || (compact_len(&document), document_sections(&document)))
            .await
            .map_err(|error| std::io::Error::other(format!("report task failed: {error}")))?;
    let mut lanes = Vec::new();
    let user_root = storage.user_root();
    for lane in Lane::ALL {
        let lane_path = user_memory_lanes::lane_path(&user_root, lane);
        let Ok(metadata) = tokio::fs::metadata(&lane_path).await else {
            continue;
        };
        let rows = user_memory_lanes::load_lane(storage, lane).await?;
        let dates: Vec<String> = rows.iter().filter_map(item_date).collect();
        lanes.push(LaneReport {
            name: lane.name().to_string(),
            rows: rows.len(),
            file_bytes: metadata.len(),
            oldest: dates.iter().min().cloned(),
            newest: dates.iter().max().cloned(),
        });
    }
    let waiting_for_review_total = sections
        .iter()
        .filter(|section| !section.name.starts_with("user."))
        .filter(|section| !super::is_agent_observed_section(&section.name))
        .map(|section| section.waiting_for_review)
        .sum();
    Ok(UserMemoryReport {
        file_bytes,
        compact_bytes,
        hot_file_target_bytes: settings.hot_file_target_bytes,
        over_target: file_bytes > settings.hot_file_target_bytes,
        mode: format!("{:?}", settings.mode).to_lowercase(),
        sections,
        waiting_for_review_total,
        lanes,
        archive: archive_report(&user_root),
    })
}

/// A plain-text rendering for the terminal. Names and numbers only.
pub fn render(report: &UserMemoryReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "knowledge.json: {} bytes on disk ({} compact), target {} — {}\nbounds mode: {}\n\n",
        report.file_bytes,
        report.compact_bytes,
        report.hot_file_target_bytes,
        if report.over_target {
            "OVER target"
        } else {
            "within target"
        },
        report.mode
    ));
    out.push_str(&format!(
        "{:<28} {:>7} {:>7} {:>10} {:>7} {:>10} {:>7} {:>7}  {:<10} {:<10}\n",
        "section",
        "entries",
        "keys",
        "bytes",
        "older",
        "older_b",
        "waiting",
        "pre_rev",
        "oldest",
        "newest"
    ));
    for s in &report.sections {
        out.push_str(&format!(
            "{:<28} {:>7} {:>7} {:>10} {:>7} {:>10} {:>7} {:>7}  {:<10} {:<10}\n",
            s.name,
            s.entries,
            s.distinct_keys,
            s.bytes,
            s.older_copy_entries,
            s.older_copy_bytes,
            s.waiting_for_review,
            s.pre_review_records,
            s.oldest.as_deref().unwrap_or("-"),
            s.newest.as_deref().unwrap_or("-")
        ));
    }
    out.push_str(&format!(
        "\nreview queue (reviewable sections): {}\n",
        report.waiting_for_review_total
    ));
    if report.lanes.is_empty() {
        out.push_str("lane files: none yet\n");
    }
    for lane in &report.lanes {
        out.push_str(&format!(
            "lane {:<18} {:>6} rows {:>10} bytes  {} .. {}\n",
            lane.name,
            lane.rows,
            lane.file_bytes,
            lane.oldest.as_deref().unwrap_or("-"),
            lane.newest.as_deref().unwrap_or("-")
        ));
    }
    out.push_str(&format!(
        "archive: {} files, {} bytes, months {}\n",
        report.archive.files,
        report.archive.bytes,
        if report.archive.months.is_empty() {
            "-".to_string()
        } else {
            report.archive.months.join(",")
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        json!({
            "workflows": [
                {"key":"report","value":"SECRET-WORDING-ONE","memory_saved_at":"2026-09-01T00:00:00Z","memory_review_needed":true},
                {"key":"report","value":"SECRET-WORDING-TWO","memory_saved_at":"2026-09-02T00:00:00Z","memory_review_needed":true},
                {"key":"other","value":"SECRET-WORDING-THREE"}
            ],
            "user": {"email_evidence": [
                {"key":"email:work:2026-10-01","notes":["SECRET-NOTE"],"day":"2026-10-01","memory_review_needed":true}
            ]},
            "_meta": {"learning_promotions": [{"candidate_id":"c1"}]}
        })
    }

    #[test]
    fn the_report_counts_known_numbers() {
        let sections = document_sections(&fixture());
        let workflows = sections.iter().find(|s| s.name == "workflows").unwrap();
        assert_eq!(workflows.entries, 3);
        assert_eq!(workflows.distinct_keys, 2);
        assert_eq!(workflows.older_copy_entries, 1);
        assert!(workflows.older_copy_bytes > 0);
        assert_eq!(workflows.waiting_for_review, 2);
        assert_eq!(workflows.pre_review_records, 1);
        assert_eq!(workflows.oldest.as_deref(), Some("2026-09-01"));
        assert_eq!(workflows.newest.as_deref(), Some("2026-09-02"));
        let email = sections
            .iter()
            .find(|s| s.name == "user.email_evidence")
            .unwrap();
        assert_eq!(email.entries, 1);
        let audit = sections
            .iter()
            .find(|s| s.name == "_meta.learning_promotions")
            .unwrap();
        assert_eq!(audit.entries, 1);
    }

    #[test]
    fn the_report_prints_no_record_contents() {
        let report = UserMemoryReport {
            sections: document_sections(&fixture()),
            ..Default::default()
        };
        for text in [render(&report), serde_json::to_string(&report).unwrap()] {
            assert!(!text.contains("SECRET"), "{text}");
        }
    }
}
