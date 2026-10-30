//! One-time clean-up of an existing user-memory file
//! (`magician memory-compact-user`). Dry run by default: it prints what it
//! would do and writes nothing. With `--apply` it backs the file up (and
//! checks the copy), takes the same exclusive lock the runtime uses (giving
//! up cleanly after 60 s), re-reads, and writes archive lines and lane files
//! before the new `knowledge.json`. A failure before that last write leaves
//! `knowledge.json` untouched, and a second run finds nothing to do.
//!
//! Steps, in order:
//! 1. The activity logs under the nested `user` object move to their lane
//!    files, slices of one day combined. Every original record is archived
//!    first (reason `moved_to_lane`), so nothing is lost in the combining.
//! 2. Records from before the reviewer are stamped `memory_review_version: 0`
//!    (owner decision 2026-10-18, option A).
//! 3. The bounds rules (`memory_lifecycle::bounds`): agent-made restatements
//!    stored once, section caps, review-queue caps — archive first.
//! 4. `_meta.learning_promotions` keeps its last 20 rows; older rows move to
//!    `archive/learning_promotions.jsonl`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use super::archive::{self, ArchiveEntry};
use super::settings::MagicianMemoryBoundsSettings;
use crate::magician_v2::agents::memory_lifecycle::bounds::{self, Usage};
use crate::magician_v2::agents::memory_lifecycle::{self, sources, state};
use crate::magician_v2::agents::storage::AgentStorage;
use crate::magician_v2::agents::user_memory_lanes::{self, Lane};
use crate::magician_v2::agents::AgentMemoryService;

/// Promotion audit rows kept in the main file.
pub const KEEP_LEARNING_PROMOTIONS: usize = 20;
const LOCK_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct LaneMove {
    pub records_moved: usize,
    pub slices_combined: usize,
    pub rows_after: usize,
    pub rows_archived: usize,
}

/// Counts only. Printed by the dry run and written next to the backup.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CompactSummary {
    pub applied: bool,
    pub nothing_to_do: bool,
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub lanes: BTreeMap<String, LaneMove>,
    pub pre_review_stamped: usize,
    /// `section -> reason -> records` leaving the main file by the bounds rules.
    pub archived_by_rule: BTreeMap<String, BTreeMap<String, usize>>,
    pub archive_lines: usize,
    pub learning_promotions_moved: usize,
    /// Under the rules before this change (activity logs and pre-review
    /// records counted as waiting).
    pub review_queue_before: usize,
    pub review_queue_after: usize,
    pub backup: Option<String>,
}

/// Everything a run would write.
#[derive(Debug, Clone)]
pub struct Compaction {
    pub document: Value,
    pub lanes: BTreeMap<Lane, Vec<Value>>,
    pub archive: Vec<ArchiveEntry>,
    pub promotions: Vec<Value>,
    pub summary: CompactSummary,
}

impl Compaction {
    fn has_work(&self) -> bool {
        !self.archive.is_empty()
            || !self.promotions.is_empty()
            || self.summary.pre_review_stamped > 0
            || self
                .summary
                .lanes
                .values()
                .any(|lane| lane.records_moved > 0)
    }
}

fn pretty_len(value: &Value) -> usize {
    serde_json::to_vec_pretty(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

/// The review queue as the reviewer saw it before this change.
fn queue_old_rules(document: &Value) -> usize {
    sources(document)
        .iter()
        .filter(|s| {
            state(&s.item) != "unresolved"
                || s.item.get("memory_review_needed") == Some(&json!(true))
        })
        .filter(|s| {
            s.item
                .get("memory_review_needed")
                .and_then(Value::as_bool)
                .unwrap_or(s.item.get("memory_review_version").is_none())
        })
        .filter(|s| s.item.get("app_source_eligibility").is_none())
        .count()
}

/// The review queue under the current rules.
pub fn queue_now(document: &Value) -> usize {
    sources(document)
        .iter()
        .filter(|s| {
            let section = s
                .pointer
                .trim_start_matches('/')
                .split('/')
                .next()
                .unwrap_or("");
            !super::is_agent_observed_section(section)
        })
        .filter(|s| {
            state(&s.item) != "unresolved"
                || s.item.get("memory_review_needed") == Some(&json!(true))
        })
        .filter(|s| bounds::waiting_for_review(&s.item))
        .filter(|s| s.item.get("app_source_eligibility").is_none())
        .count()
}

fn saved_at(item: &Value) -> String {
    item.get("memory_saved_at")
        .or_else(|| item.get("updated_at"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Identity of a promotion audit row, to skip rows the runtime already
/// appended to the archive.
pub fn promotion_identity(row: &Value) -> String {
    memory_lifecycle::digest(&json!([row.get("candidate_id"), row.get("promoted_at")]))
}

/// Work out the clean-up. Reads only.
pub fn plan(
    document: &Value,
    existing_lanes: &BTreeMap<Lane, Vec<Value>>,
    archived_promotions: &BTreeSet<String>,
    settings: &MagicianMemoryBoundsSettings,
    usage: Option<&Usage>,
    now: DateTime<Utc>,
) -> Compaction {
    let mut doc = document.clone();
    let mut summary = CompactSummary {
        bytes_before: pretty_len(document),
        review_queue_before: queue_old_rules(document),
        ..Default::default()
    };
    let mut archive_entries = Vec::new();
    let mut lanes = BTreeMap::new();

    // 1. Activity logs to their lane files.
    for lane in Lane::ALL {
        let moved: Vec<Value> = doc
            .get_mut("user")
            .and_then(Value::as_object_mut)
            .and_then(|user| user.remove(lane.name()))
            .and_then(|value| match value {
                Value::Array(items) => Some(items),
                _ => None,
            })
            .unwrap_or_default();
        if moved.is_empty() {
            continue;
        }
        let section = format!("user.{}", lane.name());
        archive_entries.extend(
            moved.iter().map(|record| {
                ArchiveEntry::new(&section, "moved_to_lane", record.clone(), None, now)
            }),
        );
        let mut slices: Vec<Value> = moved
            .iter()
            .filter(|record| user_memory_lanes::row_key(record).is_some())
            .cloned()
            .collect();
        slices.sort_by_key(saved_at);
        let existing = existing_lanes.get(&lane).cloned().unwrap_or_default();
        let (rows, replaced, combined) =
            user_memory_lanes::combine_all(lane, existing, slices, now);
        let (rows, windowed) = user_memory_lanes::apply_window(lane, rows, settings, now);
        summary.lanes.insert(
            lane.name().to_string(),
            LaneMove {
                records_moved: moved.len(),
                slices_combined: combined,
                rows_after: rows.len(),
                rows_archived: replaced.len() + windowed.len(),
            },
        );
        archive_entries.extend(replaced);
        archive_entries.extend(windowed);
        lanes.insert(lane, rows);
    }
    if doc
        .get("user")
        .and_then(Value::as_object)
        .is_some_and(|user| user.is_empty())
    {
        if let Some(map) = doc.as_object_mut() {
            map.remove("user");
        }
    }

    // 2. Pre-review records count as reviewed.
    summary.pre_review_stamped = bounds::stamp_pre_review_records(&mut doc);

    // 3. The bounds rules.
    let rules = bounds::plan(&doc, settings, usage, now);
    for ((section, reason), count) in rules.counts() {
        summary
            .archived_by_rule
            .entry(section)
            .or_default()
            .insert(reason.to_string(), count);
    }
    archive_entries.extend(rules.archive_entries(&doc, now));
    bounds::apply(&mut doc, &rules);

    // 4. The promotions audit keeps its last rows in the main file.
    let mut promotions = Vec::new();
    if let Some(rows) = doc
        .get_mut("_meta")
        .and_then(|meta| meta.get_mut("learning_promotions"))
        .and_then(Value::as_array_mut)
    {
        if rows.len() > KEEP_LEARNING_PROMOTIONS {
            let moved: Vec<Value> = rows
                .drain(..rows.len() - KEEP_LEARNING_PROMOTIONS)
                .collect();
            summary.learning_promotions_moved = moved.len();
            promotions = moved
                .into_iter()
                .filter(|row| !archived_promotions.contains(&promotion_identity(row)))
                .collect();
        }
    }

    summary.archive_lines = archive_entries.len();
    summary.bytes_after = pretty_len(&doc);
    summary.review_queue_after = queue_now(&doc);
    let mut compaction = Compaction {
        document: doc,
        lanes,
        archive: archive_entries,
        promotions,
        summary,
    };
    compaction.summary.nothing_to_do =
        !compaction.has_work() && compaction.summary.learning_promotions_moved == 0;
    compaction
}

/// The scope's `memory_backups/` directory (beside `memory/`).
pub fn backups_root(storage: &AgentStorage) -> PathBuf {
    let root = storage.root();
    root.parent().unwrap_or(root).join("memory_backups")
}

/// Identities of the promotion audit rows already in the archive.
pub fn archived_promotion_ids(user_root: &Path) -> BTreeSet<String> {
    let path = archive::archive_dir(user_root).join(archive::LEARNING_PROMOTIONS_FILE);
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .map(|row| promotion_identity(&row))
                .collect()
        })
        .unwrap_or_default()
}

async fn load_lanes(
    storage: &AgentStorage,
) -> Result<BTreeMap<Lane, Vec<Value>>, crate::magician_v2::agents::storage::AgentStorageError> {
    let mut lanes = BTreeMap::new();
    for lane in Lane::ALL {
        lanes.insert(lane, user_memory_lanes::load_lane(storage, lane).await?);
    }
    Ok(lanes)
}

/// Run the clean-up for one scope. `apply: false` writes nothing.
pub async fn run(
    service: &AgentMemoryService,
    settings: &MagicianMemoryBoundsSettings,
    apply: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<CompactSummary> {
    use anyhow::Context;
    let storage = service.storage();
    let user_root = storage.user_root();
    let usage = super::load_usage(storage).await;
    if !apply {
        let document = service.load_user_knowledge().await?;
        let lanes = load_lanes(storage).await?;
        let promoted = archived_promotion_ids(&user_root);
        let settings = settings.clone();
        let compaction = tokio::task::spawn_blocking(move || {
            plan(&document, &lanes, &promoted, &settings, usage.as_ref(), now)
        })
        .await?;
        return Ok(compaction.summary);
    }

    // 1. The runtime's lock, bounded; the lane locks after it (the runtime
    //    never holds both, so the order cannot deadlock).
    let knowledge_path = storage.user_knowledge_path();
    let _flock = tokio::time::timeout(
        LOCK_WAIT,
        AgentStorage::acquire_file_lock_exclusive(&knowledge_path),
    )
    .await
    .map_err(|_| anyhow::anyhow!("knowledge.json stayed locked for 60 s; nothing changed"))?
    .context("taking the knowledge.json lock; nothing changed")?;
    let mut lane_locks = Vec::new();
    for lane in Lane::ALL {
        lane_locks.push(
            AgentStorage::acquire_file_lock_exclusive(&user_memory_lanes::lane_path(
                &user_root, lane,
            ))
            .await
            .context("taking a lane lock; nothing changed")?,
        );
    }

    // 2. Read under the locks and plan from exactly the bytes backed up.
    let original = match tokio::fs::read(&knowledge_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => b"{}".to_vec(),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", knowledge_path.display()))
        },
    };
    let lanes = load_lanes(storage).await?;
    let promoted = archived_promotion_ids(&user_root);
    let plan_settings = settings.clone();
    let bytes = original.clone();
    let compaction = tokio::task::spawn_blocking(move || {
        let document: Value = serde_json::from_slice(&bytes)?;
        Ok::<_, serde_json::Error>(plan(
            &document,
            &lanes,
            &promoted,
            &plan_settings,
            usage.as_ref(),
            now,
        ))
    })
    .await??;
    let mut summary = compaction.summary.clone();
    summary.applied = true;
    if compaction.summary.nothing_to_do {
        return Ok(summary);
    }

    // 3. Back up the file and check the copy.
    let backup_dir = backups_root(storage).join(format!(
        "knowledge-before-growth-bounds-{}",
        now.format("%Y%m%dT%H%M%SZ")
    ));
    let backup_path = backup_dir.join("knowledge.json");
    {
        // Durable before anything else changes: the file and its directory
        // entry are flushed, then the copy is read back and compared.
        let (dir, path, bytes) = (backup_dir.clone(), backup_path.clone(), original.clone());
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::io::Write;
            std::fs::create_dir_all(&dir)?;
            let mut file = std::fs::File::create(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::File::open(&dir)?.sync_all()?;
            if let Some(parent) = dir.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        })
        .await?
        .context("writing the backup; nothing changed")?;
    }
    let copy = tokio::fs::read(&backup_path).await?;
    if blake3::hash(&copy) != blake3::hash(&original) {
        anyhow::bail!(
            "backup copy does not match {}; nothing changed",
            knowledge_path.display()
        );
    }
    summary.backup = Some(backup_path.display().to_string());

    // 4. Archive and lanes first, then the main file.
    archive::append_batch(
        &user_root,
        &compaction.archive,
        now,
        settings.archive.compress_after_months,
    )
    .await
    .context("writing archive lines; knowledge.json unchanged")?;
    for (lane, rows) in &compaction.lanes {
        write_lane_rows(storage, *lane, rows.clone())
            .await
            .context("writing a lane file; knowledge.json unchanged")?;
    }
    archive::append_values(
        &user_root,
        archive::LEARNING_PROMOTIONS_FILE,
        &compaction.promotions,
    )
    .await
    .context("archiving promotion audit rows; knowledge.json unchanged")?;
    drop(lane_locks);
    service
        .persist_user_knowledge_archived_as(
            &compaction.document,
            "growth_bounds_migration",
            &compaction.archive,
        )
        .await
        .context("writing knowledge.json")?;

    // 5. Counts next to the backup.
    write_summary(&backup_dir, &summary).await?;
    Ok(summary)
}

async fn write_lane_rows(
    storage: &AgentStorage,
    lane: Lane,
    rows: Vec<Value>,
) -> Result<(), crate::magician_v2::agents::storage::AgentStorageError> {
    let file = json!({
        "schema_version": user_memory_lanes::LANE_FILE_SCHEMA_VERSION,
        "lane": lane.name(),
        "rows": rows,
    });
    let bytes = serde_json::to_vec_pretty(&file)?;
    storage
        .write_bytes_atomic(
            user_memory_lanes::lane_path(&storage.user_root(), lane),
            &bytes,
        )
        .await
}

async fn write_summary(dir: &Path, summary: &CompactSummary) -> anyhow::Result<()> {
    tokio::fs::write(
        dir.join("growth-bounds-summary.json"),
        serde_json::to_vec_pretty(summary)?,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::agents::memory_lifecycle::merge_collection;
    use crate::magician_v2::agents::AgentMemoryResolver;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A small fixture shaped like the live file: day slices of email and
    /// chat evidence under `user`, reworded workflows, pre-review records, an
    /// owner preference and a long promotions audit. Fictional data.
    fn fixture() -> Value {
        let slice = |n: usize, subject: &str| {
            json!({"key":"email:work:2026-10-15","account":"work","day":"2026-10-15",
                "source_type":"email_capture","subjects":[subject],"senders":["Example Sender"],
                "thread_count":n,"needs_reply_count":0,"notes":[format!("note {subject}")],
                "summary":format!("{n} email threads on 2026-10-15 for work")})
        };
        let email = merge_collection(
            &[],
            &[slice(1, "Quarterly plan"), slice(2, "Venue booking")],
            "user.email_evidence",
            now(),
        );
        let chat = merge_collection(
            &[],
            &[
                json!({"key":"chat:self:2026-10-16","account":"self","day":"2026-10-16",
                "source_type":"chat_capture","thread_count":1,"summary":"1 chat thread on 2026-10-16 for self"}),
            ],
            "user.chat_evidence",
            now(),
        );
        let reworded: Vec<Value> = (0..6)
            .map(|i| {
                json!({"key":"weekly-report","value":format!("Send the weekly report, version {i}"),
                    "source_type":"inferred","updated_at":format!("2026-10-0{}T09:00:00+00:00", i + 1)})
            })
            .collect();
        let mut workflows = Vec::new();
        for item in reworded {
            workflows = merge_collection(&workflows, &[item], "workflows", now());
        }
        for (index, item) in workflows.iter_mut().enumerate() {
            item["memory_saved_at"] = json!(format!("2026-10-0{}T09:00:00+00:00", index + 1));
        }
        workflows.push(json!({"key":"legacy-step","value":"An old step"}));
        let preferences = merge_collection(
            &[],
            &[json!({"key":"units","value":"Celsius","source_type":"explicit_user_statement"})],
            "preferences",
            now(),
        );
        let promotions: Vec<Value> = (0..25)
            .map(|i| json!({"candidate_id":format!("cand-{i}"),"promoted_at":format!("2026-10-01T00:00:{i:02}Z")}))
            .collect();
        json!({
            "workflows": workflows,
            "preferences": preferences,
            "user": {"email_evidence": email, "chat_evidence": chat,
                "channel_writing_preferences": [{"key":"tone","value":"Short replies"}]},
            "_meta": {"learning_promotions": promotions},
        })
    }

    fn settings() -> MagicianMemoryBoundsSettings {
        MagicianMemoryBoundsSettings::default()
    }

    #[test]
    fn the_plan_moves_folds_stamps_and_trims() {
        let compaction = plan(
            &fixture(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            &settings(),
            None,
            now(),
        );
        let summary = &compaction.summary;
        assert_eq!(summary.lanes["email_evidence"].records_moved, 2);
        assert_eq!(summary.lanes["email_evidence"].rows_after, 1);
        assert_eq!(summary.lanes["email_evidence"].slices_combined, 1);
        assert_eq!(summary.lanes["chat_evidence"].rows_after, 1);
        assert_eq!(
            summary.archived_by_rule["workflows"]["restated_by_producer"],
            5
        );
        assert_eq!(summary.pre_review_stamped, 1);
        assert_eq!(summary.learning_promotions_moved, 5);
        assert!(summary.bytes_after < summary.bytes_before);
        assert!(summary.review_queue_after < summary.review_queue_before);
        let row = &compaction.lanes[&Lane::EmailEvidence][0];
        assert_eq!(row["thread_count"], 3);
        assert_eq!(row["subjects"], json!(["Quarterly plan", "Venue booking"]));
        // The nested object keeps only what is not an activity log, so the
        // memory index no longer sees one multi-megabyte `user` entry.
        let user = compaction.document["user"].as_object().unwrap();
        assert_eq!(
            user.keys().collect::<Vec<_>>(),
            vec!["channel_writing_preferences"]
        );
        assert_eq!(
            compaction.document["_meta"]["learning_promotions"]
                .as_array()
                .unwrap()
                .len(),
            KEEP_LEARNING_PROMOTIONS
        );
    }

    #[tokio::test]
    async fn dry_run_and_apply_agree_and_a_second_apply_finds_nothing() {
        let root = tempfile::tempdir().unwrap();
        let service = AgentMemoryResolver::new(root.path())
            .resolve_for_scope("p", "w")
            .unwrap();
        let original = fixture();
        service.save_user_knowledge(&original).await.unwrap();
        let before_bytes = tokio::fs::read(service.storage().user_knowledge_path())
            .await
            .unwrap();

        let dry = run(&service, &settings(), false, now()).await.unwrap();
        // The dry run wrote nothing.
        assert_eq!(
            tokio::fs::read(service.storage().user_knowledge_path())
                .await
                .unwrap(),
            before_bytes
        );
        assert!(archive::month_files(&service.storage().user_root()).is_empty());

        let applied = run(&service, &settings(), true, now()).await.unwrap();
        let mut comparable = applied.clone();
        comparable.applied = false;
        comparable.backup = None;
        assert_eq!(comparable, dry);
        let backup = applied.backup.clone().unwrap();
        assert_eq!(tokio::fs::read(&backup).await.unwrap(), before_bytes);

        // Every original record is live, in a lane, or in the archive.
        let after = service.load_user_knowledge().await.unwrap();
        let archived: Vec<ArchiveEntry> = archive::month_files(&service.storage().user_root())
            .iter()
            .flat_map(|path| archive::read_file(path).unwrap().0)
            .collect();
        let archived_ids: BTreeSet<&str> = archived
            .iter()
            .map(|entry| entry.record_id.as_str())
            .collect();
        let live_ids: BTreeSet<String> = sources(&after)
            .iter()
            .filter_map(|s| s.item["memory_record_id"].as_str().map(str::to_owned))
            .collect();
        for section in ["workflows", "preferences"] {
            for record in original[section].as_array().unwrap() {
                let id = record["memory_record_id"].as_str();
                let found = id.is_some_and(|id| archived_ids.contains(id) || live_ids.contains(id))
                    || after[section].as_array().unwrap().iter().any(|item| {
                        item["key"] == record["key"] && item["value"] == record["value"]
                    });
                assert!(found, "{section} record lost");
            }
        }
        for lane in ["email_evidence", "chat_evidence"] {
            for record in original["user"][lane].as_array().unwrap() {
                let id = record["memory_record_id"].as_str().unwrap();
                assert!(archived_ids.contains(id), "{lane} record not archived");
            }
        }
        assert_eq!(
            user_memory_lanes::load_lane(service.storage(), Lane::EmailEvidence)
                .await
                .unwrap()
                .len(),
            1
        );

        let again = run(&service, &settings(), true, now()).await.unwrap();
        assert!(again.nothing_to_do, "{again:?}");
        assert_eq!(service.load_user_knowledge().await.unwrap(), after);
    }
}
