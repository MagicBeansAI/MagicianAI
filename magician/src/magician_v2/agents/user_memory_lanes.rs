//! Activity-log lanes: the raw evidence the channel watchers write, kept out
//! of `knowledge.json`.
//!
//! Email evidence, chat evidence, channel patterns and channel feedback are
//! raw material, not memory: no prompt or owner surface reads them from the
//! main file, and they were 45% of it. Under `memory.user_knowledge_bounds`
//! `mode: enforce` each lives in `memory/users/lanes/<name>.json` with its own
//! lock (`lanes/.<name>.json.flock`), so a two-minute email write no longer
//! rewrites the multi-megabyte main file under its lock.
//!
//! Rows carry no lifecycle fields and are never queued for review. One row
//! per key: an email/chat row is one `(account, day)`, and each later slice of
//! that day is combined into it (lists merged without repeats, counts added,
//! notes capped). A channel pattern is one row per topic with
//! `lane_first_seen_at` and `seen_count`. Rows past the lane's time window or
//! row cap move to the archive (`user_memory_bounds::archive`); nothing is
//! deleted.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::storage::{AgentStorage, AgentStorageError};
use super::user_memory_bounds::archive::{self, ArchiveEntry, LANE_SECTION_PREFIX};
use super::user_memory_bounds::settings::MagicianMemoryBoundsSettings;
use crate::magician_v2::evidence::tier_contracts::{
    CHANNEL_FEEDBACK_TIER_NAME, CHANNEL_PATTERNS_TIER_NAME, CHAT_EVIDENCE_TIER_NAME,
    EMAIL_EVIDENCE_TIER_NAME,
};

pub const LANES_DIR: &str = "lanes";
pub const LANE_FILE_SCHEMA_VERSION: u32 = 1;
/// Per-row detail list cap, matching the bridges' per-group cap.
pub const MAX_NOTES_PER_ROW: usize = 12;
/// Set on a row restored from the archive: the window and row cap never
/// evict it again (the lane's counterpart of protected records).
pub const RESTORED_MARKER: &str = "lane_restored_at";
/// Slice fingerprints kept per row, so replaying a pass changes nothing.
const MAX_SLICE_FINGERPRINTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    EmailEvidence,
    ChatEvidence,
    ChannelPatterns,
    ChannelFeedback,
}

impl Lane {
    pub const ALL: [Lane; 4] = [
        Lane::EmailEvidence,
        Lane::ChatEvidence,
        Lane::ChannelPatterns,
        Lane::ChannelFeedback,
    ];

    /// The registry suffix name, which is also the lane file stem.
    pub fn name(self) -> &'static str {
        match self {
            Lane::EmailEvidence => EMAIL_EVIDENCE_TIER_NAME,
            Lane::ChatEvidence => CHAT_EVIDENCE_TIER_NAME,
            Lane::ChannelPatterns => CHANNEL_PATTERNS_TIER_NAME,
            Lane::ChannelFeedback => CHANNEL_FEEDBACK_TIER_NAME,
        }
    }

    pub fn from_name(name: &str) -> Option<Lane> {
        Lane::ALL.into_iter().find(|lane| lane.name() == name)
    }

    /// The lane a user-memory tier name addresses, in either spelling
    /// (`user.email_evidence` or `email_evidence`).
    pub fn for_tier(tier: &str) -> Option<Lane> {
        let tier = tier.trim().to_ascii_lowercase();
        Lane::from_name(tier.strip_prefix("user.").unwrap_or(&tier))
    }

    /// `from_section` for this lane's archive entries.
    pub fn archive_section(self) -> String {
        format!("{LANE_SECTION_PREFIX}{}", self.name())
    }
}

pub fn lanes_dir(user_root: &Path) -> PathBuf {
    user_root.join(LANES_DIR)
}

pub fn lane_path(user_root: &Path, lane: Lane) -> PathBuf {
    lanes_dir(user_root).join(format!("{}.json", lane.name()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LaneFile {
    schema_version: u32,
    lane: String,
    #[serde(default)]
    rows: Vec<Value>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct LaneUpsertOutcome {
    pub inserted: usize,
    pub combined: usize,
    pub unchanged: usize,
    pub archived: usize,
    pub rows: usize,
}

/// Read a lane's rows. A missing file is an empty lane.
pub async fn load_lane(
    storage: &AgentStorage,
    lane: Lane,
) -> Result<Vec<Value>, AgentStorageError> {
    let path = lane_path(&storage.user_root(), lane);
    match tokio::fs::metadata(&path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        _ => {},
    }
    let content = match storage.read_to_string(&path).await {
        Ok(content) => content,
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        },
        Err(error) => return Err(error),
    };
    let file: LaneFile = tokio::task::spawn_blocking(move || serde_json::from_str(&content))
        .await
        .map_err(|error| {
            AgentStorageError::Io(std::io::Error::other(format!(
                "lane parse task failed: {error}"
            )))
        })??;
    Ok(file.rows)
}

/// Upsert `rows` (each carrying `key`) into `lane` with one locked write,
/// combining each with the existing row of the same key, then apply the
/// lane's window and row cap. Archive lines are written before the lane
/// file, so a crash between the two can only leave a record in both places.
pub async fn upsert_rows(
    storage: &AgentStorage,
    lane: Lane,
    rows: Vec<Value>,
    settings: &MagicianMemoryBoundsSettings,
    now: DateTime<Utc>,
) -> Result<LaneUpsertOutcome, AgentStorageError> {
    let user_root = storage.user_root();
    let path = lane_path(&user_root, lane);
    let _flock = AgentStorage::acquire_file_lock_exclusive(&path).await?;
    let mut existing = load_lane(storage, lane).await?;
    let mut outcome = LaneUpsertOutcome::default();
    let mut archived = Vec::new();
    let mut changed = false;
    for incoming in rows {
        let Some(key) = row_key(&incoming).map(str::to_owned) else {
            continue;
        };
        match existing
            .iter_mut()
            .find(|row| row_key(row) == Some(key.as_str()))
        {
            Some(current) => match combine(lane, current, &incoming, now) {
                Combined::Unchanged => outcome.unchanged += 1,
                Combined::Row { row, archive } => {
                    if let Some((record, reason)) = archive {
                        archived.push(ArchiveEntry::new(
                            &lane.archive_section(),
                            reason,
                            record,
                            None,
                            now,
                        ));
                    }
                    *current = row;
                    outcome.combined += 1;
                    changed = true;
                },
            },
            None => {
                existing.push(new_row(lane, incoming, now));
                outcome.inserted += 1;
                changed = true;
            },
        }
    }
    let (kept, windowed) = apply_window(lane, existing, settings, now);
    if !windowed.is_empty() {
        changed = true;
    }
    archived.extend(windowed);
    outcome.archived = archived.len();
    outcome.rows = kept.len();
    if !changed {
        return Ok(outcome);
    }
    archive::append_batch(
        &user_root,
        &archived,
        now,
        settings.archive.compress_after_months,
    )
    .await?;
    write_lane(storage, lane, kept).await?;
    Ok(outcome)
}

async fn write_lane(
    storage: &AgentStorage,
    lane: Lane,
    rows: Vec<Value>,
) -> Result<(), AgentStorageError> {
    let file = LaneFile {
        schema_version: LANE_FILE_SCHEMA_VERSION,
        lane: lane.name().to_string(),
        rows,
    };
    let bytes = tokio::task::spawn_blocking(move || serde_json::to_vec_pretty(&file))
        .await
        .map_err(|error| {
            AgentStorageError::Io(std::io::Error::other(format!(
                "lane serialisation task failed: {error}"
            )))
        })??;
    storage
        .write_bytes_atomic(lane_path(&storage.user_root(), lane), &bytes)
        .await
}

pub fn row_key(row: &Value) -> Option<&str> {
    row.get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
}

/// What combining an incoming slice with an existing row produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Combined {
    /// The slice was already merged (a replayed pass).
    Unchanged,
    /// The new row, and what goes to the archive with its reason: the
    /// previous row when its wording was replaced (`lane_row_replaced`), or
    /// the incoming slice when combining dropped something from it — notes
    /// past the cap, or a value that differs from the row's
    /// (`lane_slice_combined`).
    Row {
        row: Value,
        archive: Option<(Value, &'static str)>,
    },
}

/// Fields that are row bookkeeping, not content, for slice fingerprints.
fn is_volatile(field: &str) -> bool {
    matches!(field, "updated_at" | "key")
        || field.starts_with("lane_")
        || field.starts_with("memory_")
        || field == "seen_count"
}

/// The fingerprint of one written slice: its content without bookkeeping.
pub fn slice_fingerprint(row: &Value) -> String {
    let content: Map<String, Value> = row
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(field, _)| !is_volatile(field))
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    crate::magician_v2::agents::memory_lifecycle::digest(&Value::Object(content))
}

fn fingerprints(row: &Value) -> Vec<String> {
    row.get("lane_slices")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn push_fingerprint(row: &mut Value, fingerprint: String) {
    let mut all = fingerprints(row);
    if !all.contains(&fingerprint) {
        all.push(fingerprint);
    }
    if all.len() > MAX_SLICE_FINGERPRINTS {
        let excess = all.len() - MAX_SLICE_FINGERPRINTS;
        all.drain(0..excess);
    }
    row["lane_slices"] = json!(all);
}

/// A row as first stored: lifecycle fields stripped (lanes are not
/// reviewed), first-seen time and slice fingerprint stamped.
pub fn new_row(lane: Lane, incoming: Value, now: DateTime<Utc>) -> Value {
    let mut row = strip_lifecycle(incoming);
    let fingerprint = slice_fingerprint(&row);
    if lane == Lane::ChannelPatterns && row.get("seen_count").is_none() {
        row["seen_count"] = json!(1);
    }
    row["lane_first_seen_at"] = json!(now.to_rfc3339());
    row["lane_updated_at"] = json!(now.to_rfc3339());
    push_fingerprint(&mut row, fingerprint);
    row
}

/// Remove the lifecycle envelope a record picked up in `knowledge.json`.
pub fn strip_lifecycle(mut row: Value) -> Value {
    if let Some(object) = row.as_object_mut() {
        object.retain(|field, _| !field.starts_with("memory_"));
    }
    row
}

/// Combine `incoming` into `existing` (same key) by the lane's rule.
pub fn combine(lane: Lane, existing: &Value, incoming: &Value, now: DateTime<Utc>) -> Combined {
    let incoming = strip_lifecycle(incoming.clone());
    let fingerprint = slice_fingerprint(&incoming);
    if fingerprints(existing).contains(&fingerprint) {
        return Combined::Unchanged;
    }
    let (mut row, archive) = match lane {
        Lane::EmailEvidence | Lane::ChatEvidence => {
            const ADDITIVE: &[&str] = &["thread_count", "needs_reply_count"];
            let mut row = combine_fields(existing, &incoming, ADDITIVE);
            row["summary"] = json!(evidence_summary(lane, &row));
            let lossy = slice_loses_detail(existing, &incoming, &row, ADDITIVE);
            (
                row,
                lossy.then(|| (incoming.clone(), "lane_slice_combined")),
            )
        },
        Lane::ChannelFeedback => {
            const ADDITIVE: &[&str] = &["did", "acknowledged", "dismissed"];
            let mut row = combine_fields(existing, &incoming, ADDITIVE);
            row["dismiss_reasons"] = add_counts(
                existing.get("dismiss_reasons"),
                incoming.get("dismiss_reasons"),
            );
            row["summary"] = json!(feedback_summary(&row));
            let lossy = slice_loses_detail(existing, &incoming, &row, ADDITIVE);
            (
                row,
                lossy.then(|| (incoming.clone(), "lane_slice_combined")),
            )
        },
        Lane::ChannelPatterns => {
            // A pattern is restated, not sliced: the newest wording is the
            // row; the previous wording goes to the archive.
            let mut row = incoming.clone();
            for field in ["key", "lane_first_seen_at", "lane_slices"] {
                if let Some(value) = existing.get(field) {
                    row[field] = value.clone();
                }
            }
            let first_seen = existing
                .get("first_seen_at")
                .cloned()
                .or_else(|| existing.get("synthesized_at").cloned())
                .unwrap_or_else(|| json!(now.to_rfc3339()));
            row["first_seen_at"] = first_seen;
            let seen = existing
                .get("seen_count")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            row["seen_count"] = json!(seen + 1);
            let restated = ["note", "examples", "timing", "topic"]
                .iter()
                .any(|field| existing.get(*field) != incoming.get(*field));
            (
                row,
                restated.then(|| (existing.clone(), "lane_row_replaced")),
            )
        },
    };
    row["lane_updated_at"] = json!(now.to_rfc3339());
    push_fingerprint(&mut row, fingerprint);
    Combined::Row { row, archive }
}

/// Whether combining `incoming` into `existing` dropped detail from it: a
/// list item (a note past the cap) missing from the combined row, or a plain
/// value that replaced a different one. Counts are added, and the summary is
/// recomputed, so neither loses anything.
fn slice_loses_detail(existing: &Value, incoming: &Value, row: &Value, additive: &[&str]) -> bool {
    let Some(fields) = incoming.as_object() else {
        return false;
    };
    fields.iter().any(|(field, value)| {
        if is_volatile(field)
            || additive.contains(&field.as_str())
            || matches!(field.as_str(), "summary" | "dismiss_reasons")
        {
            return false;
        }
        match (value, row.get(field)) {
            (Value::Array(items), Some(Value::Array(kept))) => {
                items.iter().any(|item| !kept.contains(item))
            },
            _ => existing
                .get(field)
                .is_some_and(|old| !old.is_null() && old != value),
        }
    })
}

/// Field-wise slice combination: lists merged without repeats (notes capped),
/// listed counts added, every other field taken from the newer slice.
fn combine_fields(existing: &Value, incoming: &Value, additive: &[&str]) -> Value {
    let mut row = existing.clone();
    let Some(incoming) = incoming.as_object() else {
        return row;
    };
    for (field, value) in incoming {
        let merged = match (existing.get(field), value) {
            (Some(Value::Array(old)), Value::Array(new)) => {
                let mut items = old.clone();
                for item in new {
                    if !items.contains(item) {
                        items.push(item.clone());
                    }
                }
                if field == "notes" {
                    items.truncate(MAX_NOTES_PER_ROW);
                }
                Value::Array(items)
            },
            (Some(old), new) if additive.contains(&field.as_str()) => {
                json!(old.as_u64().unwrap_or(0) + new.as_u64().unwrap_or(0))
            },
            _ => value.clone(),
        };
        row[field.as_str()] = merged;
    }
    row
}

fn add_counts(old: Option<&Value>, new: Option<&Value>) -> Value {
    let mut out = std::collections::BTreeMap::<String, u64>::new();
    for map in [old, new]
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        for (reason, count) in map {
            *out.entry(reason.clone()).or_default() += count.as_u64().unwrap_or(0);
        }
    }
    json!(out)
}

fn str_field<'a>(row: &'a Value, field: &str) -> &'a str {
    row.get(field).and_then(Value::as_str).unwrap_or("")
}

/// The evidence bridge's headline, recomputed for a combined day row. Must
/// match `magician-comms` `evidence_bridge::build_bridge_rows`.
pub fn evidence_summary(lane: Lane, row: &Value) -> String {
    let threads = row.get("thread_count").and_then(Value::as_u64).unwrap_or(0);
    let word = if lane == Lane::EmailEvidence {
        "email"
    } else {
        "chat"
    };
    format!(
        "{threads} {word} thread{} on {} for {}",
        if threads == 1 { "" } else { "s" },
        str_field(row, "day"),
        str_field(row, "account")
    )
}

/// The feedback bridge's summary, recomputed for a combined day row. Must
/// match `magician-comms` `feedback_bridge::build_feedback_rows`.
pub fn feedback_summary(row: &Value) -> String {
    let count = |field: &str| row.get(field).and_then(Value::as_u64).unwrap_or(0);
    let reasons: Vec<String> = row
        .get("dismiss_reasons")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(reason, n)| format!("{reason}×{}", n.as_u64().unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    format!(
        "On {} for {}: acted on {}, acknowledged {}, dismissed {}{}.",
        str_field(row, "day"),
        str_field(row, "account"),
        count("did"),
        count("acknowledged"),
        count("dismissed"),
        if reasons.is_empty() {
            String::new()
        } else {
            format!(" (reasons: {})", reasons.join(", "))
        },
    )
}

/// When a row last carried news: its `day`, else when the lane last
/// changed it, else when it was written. `None` keeps the row.
fn row_age_date(row: &Value) -> Option<NaiveDate> {
    if let Some(day) = row
        .get("day")
        .or_else(|| row.get("date"))
        .and_then(Value::as_str)
    {
        if let Ok(date) = NaiveDate::parse_from_str(day.get(..10).unwrap_or(day), "%Y-%m-%d") {
            return Some(date);
        }
    }
    for field in ["lane_updated_at", "updated_at"] {
        if let Some(at) = row
            .get(field)
            .and_then(Value::as_str)
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        {
            return Some(at.date_naive());
        }
    }
    row.get("synthesized_at")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .map(|at| at.date_naive())
}

/// Split rows into those kept and archive entries for rows past the lane's
/// time window, then for the oldest rows beyond its row cap.
pub fn apply_window(
    lane: Lane,
    rows: Vec<Value>,
    settings: &MagicianMemoryBoundsSettings,
    now: DateTime<Utc>,
) -> (Vec<Value>, Vec<ArchiveEntry>) {
    let cutoff = (now - chrono::Duration::days(i64::from(settings.lanes.hot_days))).date_naive();
    let section = lane.archive_section();
    let mut archived = Vec::new();
    let mut kept = Vec::new();
    for row in rows {
        if row.get(RESTORED_MARKER).is_none()
            && row_age_date(&row).is_some_and(|date| date < cutoff)
        {
            archived.push(ArchiveEntry::new(&section, "lane_window", row, None, now));
        } else {
            kept.push(row);
        }
    }
    if let Some(cap) = settings.lane_max_rows(lane.name()) {
        // Restored rows neither count toward the cap nor get evicted by it.
        let mut order: Vec<usize> = (0..kept.len())
            .filter(|&index| kept[index].get(RESTORED_MARKER).is_none())
            .collect();
        if order.len() > cap {
            // Oldest first; undated rows count as newest.
            order.sort_by_key(|&index| {
                (row_age_date(&kept[index]).unwrap_or(NaiveDate::MAX), index)
            });
            let excess = order.len() - cap;
            let drop: std::collections::BTreeSet<usize> = order[..excess].iter().copied().collect();
            let mut survivors = Vec::with_capacity(cap);
            for (index, row) in kept.into_iter().enumerate() {
                if drop.contains(&index) {
                    archived.push(ArchiveEntry::new(&section, "lane_row_cap", row, None, now));
                } else {
                    survivors.push(row);
                }
            }
            kept = survivors;
        }
    }
    (kept, archived)
}

/// Combine many `(key, row)` slices into lane rows in memory (the migration
/// path). Returns the rows and the archive entries for replaced wordings.
pub fn combine_all(
    lane: Lane,
    existing: Vec<Value>,
    incoming: impl IntoIterator<Item = Value>,
    now: DateTime<Utc>,
) -> (Vec<Value>, Vec<ArchiveEntry>, usize) {
    let mut rows = existing;
    let mut archived = Vec::new();
    let mut combined = 0;
    for slice in incoming {
        let Some(key) = row_key(&slice).map(str::to_owned) else {
            continue;
        };
        match rows
            .iter_mut()
            .find(|row| row_key(row) == Some(key.as_str()))
        {
            Some(current) => {
                if let Combined::Row { row, archive } = combine(lane, current, &slice, now) {
                    if let Some((record, reason)) = archive {
                        archived.push(ArchiveEntry::new(
                            &lane.archive_section(),
                            reason,
                            record,
                            None,
                            now,
                        ));
                    }
                    *current = row;
                    combined += 1;
                }
            },
            None => rows.push(new_row(lane, slice, now)),
        }
    }
    (rows, archived, combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::agents::AgentMemoryResolver;

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn email_slice(subjects: &[&str], threads: u64, notes: &[&str]) -> Value {
        json!({"key":"email:work:2026-10-17","account":"work","day":"2026-10-17",
            "source_type":"email_capture","subjects":subjects,"senders":["Example Sender"],
            "thread_count":threads,"needs_reply_count":1,"notes":notes,
            "summary":format!("{threads} email threads on 2026-10-17 for work")})
    }

    fn storage(root: &Path) -> AgentStorage {
        AgentMemoryResolver::new(root)
            .resolve_for_scope("p", "w")
            .unwrap()
            .storage()
            .clone()
    }

    fn enforce() -> MagicianMemoryBoundsSettings {
        let mut settings = MagicianMemoryBoundsSettings::default();
        settings.mode =
            super::super::user_memory_bounds::settings::UserKnowledgeBoundsMode::Enforce;
        settings
    }

    #[test]
    fn tier_names_resolve_in_either_spelling() {
        assert_eq!(
            Lane::for_tier("user.email_evidence"),
            Some(Lane::EmailEvidence)
        );
        assert_eq!(
            Lane::for_tier("channel_patterns"),
            Some(Lane::ChannelPatterns)
        );
        assert_eq!(Lane::for_tier("user.channel_writing_preferences"), None);
        assert_eq!(Lane::for_tier("preferences"), None);
    }

    #[tokio::test]
    async fn two_passes_over_one_day_give_one_row_holding_both_slices() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let now = at("2026-10-17T18:00:00Z");
        let first = email_slice(&["Budget review"], 2, &["asked for the draft"]);
        let second = email_slice(&["Team offsite"], 3, &["confirmed the venue"]);
        upsert_rows(
            &storage,
            Lane::EmailEvidence,
            vec![first.clone()],
            &enforce(),
            now,
        )
        .await
        .unwrap();
        upsert_rows(
            &storage,
            Lane::EmailEvidence,
            vec![second.clone()],
            &enforce(),
            now,
        )
        .await
        .unwrap();
        let rows = load_lane(&storage, Lane::EmailEvidence).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["subjects"], json!(["Budget review", "Team offsite"]));
        assert_eq!(row["thread_count"], 5);
        assert_eq!(row["needs_reply_count"], 2);
        assert_eq!(
            row["notes"],
            json!(["asked for the draft", "confirmed the venue"])
        );
        assert_eq!(row["summary"], "5 email threads on 2026-10-17 for work");
        assert!(row.get("memory_lifecycle").is_none());

        // Replaying the same pass changes nothing.
        let replay = upsert_rows(&storage, Lane::EmailEvidence, vec![second], &enforce(), now)
            .await
            .unwrap();
        assert_eq!(replay.unchanged, 1);
        assert_eq!(
            load_lane(&storage, Lane::EmailEvidence).await.unwrap(),
            rows
        );
    }

    /// Notes past the cap (or a value replaced by a later slice) are not
    /// dropped: the slice that carried them is archived first.
    #[tokio::test]
    async fn a_slice_whose_notes_overflow_is_archived_before_combining() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let now = at("2026-10-17T18:00:00Z");
        let first_notes: Vec<String> = (0..12).map(|i| format!("first note {i}")).collect();
        let first_refs: Vec<&str> = first_notes.iter().map(String::as_str).collect();
        upsert_rows(
            &storage,
            Lane::EmailEvidence,
            vec![email_slice(&["Budget review"], 1, &first_refs)],
            &enforce(),
            now,
        )
        .await
        .unwrap();
        // Fits: nothing to archive.
        upsert_rows(
            &storage,
            Lane::EmailEvidence,
            vec![email_slice(&["Budget review"], 1, &["first note 3"])],
            &enforce(),
            now,
        )
        .await
        .unwrap();
        assert!(archive::month_files(&storage.user_root()).is_empty());
        // Overflows the 12-note cap: the slice goes to the archive.
        let overflow = email_slice(&["Team offsite"], 2, &["thirteenth note"]);
        upsert_rows(
            &storage,
            Lane::EmailEvidence,
            vec![overflow.clone()],
            &enforce(),
            now,
        )
        .await
        .unwrap();
        let rows = load_lane(&storage, Lane::EmailEvidence).await.unwrap();
        assert_eq!(
            rows[0]["notes"].as_array().unwrap().len(),
            MAX_NOTES_PER_ROW
        );
        let (lines, _) =
            archive::read_file(&archive::month_files(&storage.user_root())[0]).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].reason, "lane_slice_combined");
        assert_eq!(lines[0].record["notes"], json!(["thirteenth note"]));
    }

    #[tokio::test]
    async fn concurrent_upserts_from_two_tasks_lose_no_rows() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let now = at("2026-10-17T18:00:00Z");
        let settings = enforce();
        let a = (0..20).map(
            |i| json!({"key":format!("feedback:a:{i}"),"account":"a","day":"2026-10-17","did":1}),
        );
        let b = (0..20).map(
            |i| json!({"key":format!("feedback:b:{i}"),"account":"b","day":"2026-10-17","did":1}),
        );
        let (left, right) = tokio::join!(
            upsert_rows(&storage, Lane::ChannelFeedback, a.collect(), &settings, now),
            upsert_rows(&storage, Lane::ChannelFeedback, b.collect(), &settings, now),
        );
        left.unwrap();
        right.unwrap();
        assert_eq!(
            load_lane(&storage, Lane::ChannelFeedback)
                .await
                .unwrap()
                .len(),
            40
        );
    }

    #[tokio::test]
    async fn the_same_topic_on_two_days_is_one_row_seen_twice_and_one_archive_line() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let day_one = at("2026-10-16T09:00:00Z");
        let day_two = at("2026-10-17T09:00:00Z");
        let pattern = |note: &str, at_ms: i64| {
            json!({"key":"weekly_planning","topic":"Weekly planning","timing":"Mondays",
                "note":note,"examples":[],"source":"passive_channel_synthesis","synthesized_at":at_ms})
        };
        upsert_rows(
            &storage,
            Lane::ChannelPatterns,
            vec![pattern("Plans the week early", day_one.timestamp_millis())],
            &enforce(),
            day_one,
        )
        .await
        .unwrap();
        upsert_rows(
            &storage,
            Lane::ChannelPatterns,
            vec![pattern(
                "Plans the week on Monday morning",
                day_two.timestamp_millis(),
            )],
            &enforce(),
            day_two,
        )
        .await
        .unwrap();
        let rows = load_lane(&storage, Lane::ChannelPatterns).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["seen_count"], 2);
        assert_eq!(rows[0]["first_seen_at"], day_one.timestamp_millis());
        assert_eq!(rows[0]["note"], "Plans the week on Monday morning");
        let files = archive::month_files(&storage.user_root());
        let (lines, _) = archive::read_file(&files[0]).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].from_section, "lane:channel_patterns");
        assert_eq!(lines[0].record["note"], "Plans the week early");
    }

    #[tokio::test]
    async fn window_and_row_cap_move_rows_to_the_archive() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let now = at("2026-10-17T18:00:00Z");
        let mut settings = enforce();
        settings.lanes.max_rows.insert("chat_evidence".into(), 2);
        let row = |day: &str| {
            json!({"key":format!("chat:work:{day}"),"account":"work","day":day,
                "source_type":"chat_capture","thread_count":1})
        };
        let rows = vec![
            row("2026-08-01"), // past the 45-day window
            row("2026-10-14"),
            row("2026-10-15"),
            row("2026-10-16"), // keeps the two newest within the cap
        ];
        let outcome = upsert_rows(&storage, Lane::ChatEvidence, rows, &settings, now)
            .await
            .unwrap();
        assert_eq!(outcome.archived, 2);
        let kept = load_lane(&storage, Lane::ChatEvidence).await.unwrap();
        let days: Vec<_> = kept.iter().map(|r| r["day"].as_str().unwrap()).collect();
        assert_eq!(days, vec!["2026-10-15", "2026-10-16"]);
        let (lines, _) =
            archive::read_file(&archive::month_files(&storage.user_root())[0]).unwrap();
        let reasons: Vec<_> = lines.iter().map(|l| l.reason.as_str()).collect();
        assert_eq!(reasons, vec!["lane_window", "lane_row_cap"]);
    }

    #[tokio::test]
    async fn a_missing_lane_file_reads_as_empty() {
        let root = tempfile::tempdir().unwrap();
        assert!(load_lane(&storage(root.path()), Lane::EmailEvidence)
            .await
            .unwrap()
            .is_empty());
    }
}
