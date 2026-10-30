//! Growth bounds for the owner's user-memory file (`knowledge.json`).
//!
//! - [`settings`]: `memory.user_knowledge_bounds` (default `mode: observe`).
//! - [`archive`]: the restorable, append-only archive everything leaving the
//!   main file goes to first.
//! - [`bound_write`]: run on every `knowledge.json` write. Observe logs what
//!   would leave (at most hourly per file); enforce archives it, then applies
//!   the change. The rules are in `memory_lifecycle::bounds`.
//! - [`report`]: the read-only size report (`magician memory-user-report`).
//! - [`compact`]: the one-time clean-up (`magician memory-compact-user`).
//!
//! The activity-log lanes are `agents::user_memory_lanes`.
//! Plan: `docs/archive/plans/2026-10-18-user-memory-growth-bounds.md`.

pub mod archive;
pub mod compact;
pub mod dedupe;
pub mod report;
pub mod settings;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::{info, warn};

use super::memory_lifecycle::bounds::{self, Usage};
use super::storage::AgentStorage;
use crate::magician_v2::chat::service::{
    UserMemoryTierAudience, UserMemoryTierCuration, USER_MEMORY_TIERS,
};
pub use settings::{
    current_settings, install_settings, with_settings, MagicianMemoryBoundsSettings,
    UserKnowledgeBoundsMode,
};

/// A top-level section the bounds may govern: a registry tier that is stored
/// as a named array (the `user` root object is not one).
pub fn is_registered_section(name: &str) -> bool {
    name != "user" && USER_MEMORY_TIERS.iter().any(|tier| tier.name == name)
}

/// Whether a section holds the owner's own material (reviewed first).
pub fn is_owner_section(name: &str) -> bool {
    USER_MEMORY_TIERS
        .iter()
        .any(|tier| tier.name == name && tier.audience == UserMemoryTierAudience::Owner)
}

/// Agent-observed activity logs (the `user` subtree and the lane names):
/// raw material the lifecycle reviewer never queues.
pub fn is_agent_observed_section(name: &str) -> bool {
    USER_MEMORY_TIERS.iter().any(|tier| {
        tier.name == name
            && tier.audience == UserMemoryTierAudience::Agent
            && tier.curation == UserMemoryTierCuration::Observed
    })
}

const OBSERVE_LOG_EVERY: Duration = Duration::from_secs(3600);

fn observe_log_due(path: PathBuf) -> bool {
    static LAST: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = Instant::now();
    match last.get(&path) {
        Some(at) if now.duration_since(*at) < OBSERVE_LOG_EVERY => false,
        _ => {
            last.insert(path, now);
            true
        },
    }
}

/// Usage counts for coldest-first ordering. `None` when the overlay cannot be
/// read: the caps then fall back to oldest first.
pub async fn load_usage(storage: &AgentStorage) -> Option<Usage> {
    match crate::magician_v2::agents::memory_temperature::load_memory_temperature_overlay_snapshot(
        storage,
    )
    .await
    {
        Ok(overlay) => Some(Usage::from_overlay(&overlay)),
        Err(error) => {
            warn!(error = %error, "user memory bounds: temperature overlay unreadable; caps go oldest first");
            None
        },
    }
}

/// What [`bound_write`] did.
#[derive(Debug, Default)]
pub struct BoundedWrite {
    /// Records archived and removed from the document by this write.
    pub archived: Vec<archive::ArchiveEntry>,
}

impl BoundedWrite {
    /// Intention ids of archived records, for the preference history.
    pub fn archived_intention_ids(&self) -> BTreeSet<String> {
        archived_intention_ids(&self.archived)
    }
}

pub fn archived_intention_ids(entries: &[archive::ArchiveEntry]) -> BTreeSet<String> {
    entries
        .iter()
        .filter(|entry| !entry.from_section.starts_with(archive::LANE_SECTION_PREFIX))
        .map(|entry| {
            crate::magician_v2::preference_alignment::sources::native::intention_id(
                &entry.from_section,
                &entry.record,
            )
        })
        .collect()
}

/// Apply the size bounds to a `knowledge.json` document about to be written.
/// The caller holds the file's exclusive lock. Under enforce, removed records
/// are archived first; if the archive write fails, the document is written
/// unchanged. Off and observe never change the document. An error means a
/// worker task died; the caller must not write.
pub async fn bound_write(
    storage: &AgentStorage,
    document: Value,
) -> std::io::Result<(Value, BoundedWrite)> {
    let settings = current_settings();
    let now = Utc::now();
    match settings.mode {
        UserKnowledgeBoundsMode::Off => Ok((document, BoundedWrite::default())),
        UserKnowledgeBoundsMode::Observe => {
            if !observe_log_due(storage.user_knowledge_path()) {
                return Ok((document, BoundedWrite::default()));
            }
            let (document, plan) = plan_off_thread(document, settings, None, now).await?;
            log_plan("observe: would archive", &plan, &document);
            Ok((document, BoundedWrite::default()))
        },
        UserKnowledgeBoundsMode::Enforce => {
            let (document, first) = plan_off_thread(document, settings.clone(), None, now).await?;
            if first.is_empty() {
                return Ok((document, BoundedWrite::default()));
            }
            // Usage only orders cap evictions; folding does not need it.
            let needs_usage = first.removals.iter().any(|removal| {
                !matches!(removal.reason, "restated_by_producer" | "merged_duplicate")
            });
            let usage = match needs_usage {
                true => load_usage(storage).await,
                false => None,
            };
            let (document, plan) = match usage {
                Some(usage) => {
                    plan_off_thread(document, settings.clone(), Some(usage), now).await?
                },
                None => (document, first),
            };
            let entries = plan.archive_entries(&document, now);
            if let Err(error) = archive::append_batch(
                &storage.user_root(),
                &entries,
                now,
                settings.archive.compress_after_months,
            )
            .await
            {
                warn!(
                    error = %error,
                    records = entries.len(),
                    "user memory bounds: archive write failed; keeping every record"
                );
                return Ok((document, BoundedWrite::default()));
            }
            log_plan("archived", &plan, &document);
            let document = tokio::task::spawn_blocking(move || {
                let mut document = document;
                bounds::apply(&mut document, &plan);
                document
            })
            .await
            .map_err(|error| std::io::Error::other(format!("bounds apply task failed: {error}")))?;
            Ok((document, BoundedWrite { archived: entries }))
        },
    }
}

async fn plan_off_thread(
    document: Value,
    settings: std::sync::Arc<MagicianMemoryBoundsSettings>,
    usage: Option<Usage>,
    now: DateTime<Utc>,
) -> std::io::Result<(Value, bounds::Plan)> {
    tokio::task::spawn_blocking(move || {
        let plan = bounds::plan(&document, &settings, usage.as_ref(), now);
        (document, plan)
    })
    .await
    .map_err(|error| std::io::Error::other(format!("bounds plan task failed: {error}")))
}

fn log_plan(what: &str, plan: &bounds::Plan, document: &Value) {
    if plan.is_empty() {
        return;
    }
    let counts: Vec<String> = plan
        .counts()
        .into_iter()
        .map(|((section, reason), n)| format!("{section}/{reason}={n}"))
        .collect();
    let lanes_in_file = document
        .get("user")
        .and_then(Value::as_object)
        .map(|user| {
            user.keys()
                .filter(|name| super::user_memory_lanes::Lane::from_name(name).is_some())
                .count()
        })
        .unwrap_or(0);
    info!(
        records = plan.removals.len(),
        by_section = %counts.join(","),
        activity_logs_still_in_main_file = lanes_in_file,
        "user memory bounds {what}"
    );
}

/// What [`restore_record`] found or did.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct RestoreOutcome {
    pub record_id: String,
    pub found: bool,
    pub section: Option<String>,
    pub archived_at: Option<String>,
    pub reason: Option<String>,
    /// Already live: nothing to put back.
    pub already_live: bool,
    pub restored: bool,
}

/// Put an archived record back into its section (or lane), pending review
/// so it takes the normal path, and protected from the bounds from then on.
/// `apply: false` only looks. The archive line stays (the archive is
/// append-only).
pub async fn restore_record(
    service: &super::AgentMemoryService,
    record_id: &str,
    apply: bool,
) -> anyhow::Result<RestoreOutcome> {
    let user_root = service.storage().user_root();
    let lookup_root = user_root.clone();
    let id = record_id.to_string();
    let entry =
        tokio::task::spawn_blocking(move || archive::find_latest(&lookup_root, &id)).await??;
    let mut outcome = RestoreOutcome {
        record_id: record_id.to_string(),
        found: entry.is_some(),
        section: entry.as_ref().map(|e| e.from_section.clone()),
        archived_at: entry.as_ref().map(|e| e.archived_at.clone()),
        reason: entry.as_ref().map(|e| e.reason.clone()),
        already_live: false,
        restored: false,
    };
    let Some(entry) = entry else {
        return Ok(outcome);
    };
    let now = Utc::now();
    let record = archive::restorable_record(&entry, now);
    let lane = entry
        .from_section
        .strip_prefix(archive::LANE_SECTION_PREFIX)
        .or_else(|| entry.from_section.strip_prefix("user."))
        .and_then(super::user_memory_lanes::Lane::from_name);
    if archive::is_lane_section(&entry.from_section) {
        let lane = lane.ok_or_else(|| {
            anyhow::anyhow!("unknown lane `{}` in the archive", entry.from_section)
        })?;
        let record = super::user_memory_lanes::strip_lifecycle(record);
        let key = super::user_memory_lanes::row_key(&record).map(str::to_owned);
        let rows = super::user_memory_lanes::load_lane(service.storage(), lane).await?;
        let fingerprint = super::user_memory_lanes::slice_fingerprint(&record);
        outcome.already_live = rows
            .iter()
            .any(|row| super::user_memory_lanes::slice_fingerprint(row) == fingerprint);
        if apply && !outcome.already_live {
            super::user_memory_lanes::upsert_rows(
                service.storage(),
                lane,
                vec![record],
                &current_settings(),
                now,
            )
            .await?;
            // Restored means present after the write, not merely attempted.
            let after = super::user_memory_lanes::load_lane(service.storage(), lane).await?;
            outcome.restored = key.is_some_and(|key| {
                after.iter().any(|row| {
                    super::user_memory_lanes::row_key(row) == Some(key.as_str())
                        && row.get(super::user_memory_lanes::RESTORED_MARKER).is_some()
                })
            });
        }
        return Ok(outcome);
    }
    let section = entry.from_section.clone();
    let record_id = entry.record_id.clone();
    let mut already_live = false;
    let mut restored = false;
    let write = apply;
    service
        .update_user_knowledge_as("memory_restore", |document| {
            if !document.is_object() {
                *document = Value::Object(serde_json::Map::new());
            }
            let items = document
                .as_object_mut()
                .expect("object")
                .entry(section.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            let Some(items) = items.as_array_mut() else {
                return Err(super::memory::AgentMemoryError::Validation(format!(
                    "section `{section}` is not a list; restore it by hand"
                )));
            };
            already_live = items.iter().any(|item| {
                item.get("memory_record_id").and_then(Value::as_str) == Some(record_id.as_str())
                    && !super::memory_lifecycle::retired(item)
            });
            if already_live || !write {
                return Ok(false);
            }
            items.push(record);
            restored = true;
            Ok(true)
        })
        .await?;
    outcome.already_live = already_live;
    // Restored means present after the write (the bounds protect restored
    // records, so this holds unless something else removed it).
    outcome.restored = restored && {
        let document = service.load_user_knowledge().await?;
        document
            .get(&section)
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("memory_record_id").and_then(Value::as_str) == Some(record_id.as_str())
                        && item.get("memory_restored_at").is_some()
                })
            })
    };
    Ok(outcome)
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    use crate::magician_v2::agents::AgentMemoryResolver;
    use serde_json::json;

    #[tokio::test]
    async fn an_archived_record_goes_back_pending_review_and_only_once() {
        let root = tempfile::tempdir().unwrap();
        let service = AgentMemoryResolver::new(root.path())
            .resolve_for_scope("p", "w")
            .unwrap();
        let record = json!({"key":"weekly-report","value":"Send the report on Fridays",
            "memory_record_id":"rec-weekly","source_type":"inferred","memory_lifecycle":"active"});
        let now = Utc::now();
        archive::append_batch(
            &service.storage().user_root(),
            &[archive::ArchiveEntry::new(
                "workflows",
                "section_cap",
                record,
                None,
                now,
            )],
            now,
            3,
        )
        .await
        .unwrap();
        let look = restore_record(&service, "rec-weekly", false).await.unwrap();
        assert!(look.found && !look.restored);
        assert!(service
            .load_user_knowledge()
            .await
            .unwrap()
            .get("workflows")
            .is_none());

        let done = restore_record(&service, "rec-weekly", true).await.unwrap();
        assert!(done.restored);
        let document = service.load_user_knowledge().await.unwrap();
        let back = &document["workflows"][0];
        assert_eq!(back["memory_lifecycle"], "pending_review");
        assert!(back.get("memory_restored_at").is_some());

        let again = restore_record(&service, "rec-weekly", true).await.unwrap();
        assert!(again.already_live && !again.restored);
        assert_eq!(
            service.load_user_knowledge().await.unwrap()["workflows"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            !restore_record(&service, "missing", true)
                .await
                .unwrap()
                .found
        );
    }
}

#[cfg(test)]
mod lane_restore_tests {
    use super::*;
    use crate::magician_v2::agents::user_memory_lanes::{self, Lane, RESTORED_MARKER};
    use crate::magician_v2::agents::AgentMemoryResolver;
    use serde_json::json;

    fn chat_row(day: &str) -> Value {
        json!({"key":format!("chat:self:{day}"),"account":"self","day":day,
            "source_type":"chat_capture","thread_count":1})
    }

    /// A lane row archived by the window or the row cap comes back and
    /// stays: the restored marker exempts it from both, and `restored` is
    /// read from the lane after the write.
    #[tokio::test]
    async fn restored_lane_rows_are_not_evicted_again() {
        let root = tempfile::tempdir().unwrap();
        let service = AgentMemoryResolver::new(root.path())
            .resolve_for_scope("p", "w")
            .unwrap();
        let mut settings = MagicianMemoryBoundsSettings::default();
        settings.lanes.max_rows.insert("chat_evidence".into(), 2);
        let now = Utc::now();
        let today = now.format("%Y-%m-%d").to_string();
        let yesterday = (now - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        let recent = (now - chrono::Duration::days(3))
            .format("%Y-%m-%d")
            .to_string();
        let section = Lane::ChatEvidence.archive_section();
        let windowed =
            archive::ArchiveEntry::new(&section, "lane_window", chat_row("2026-01-05"), None, now);
        let capped =
            archive::ArchiveEntry::new(&section, "lane_row_cap", chat_row(&recent), None, now);
        archive::append_batch(
            &service.storage().user_root(),
            &[windowed.clone(), capped.clone()],
            now,
            3,
        )
        .await
        .unwrap();
        with_settings(settings.clone(), async {
            // The lane is already at its cap with newer rows.
            user_memory_lanes::upsert_rows(
                service.storage(),
                Lane::ChatEvidence,
                vec![chat_row(&yesterday), chat_row(&today)],
                &settings,
                now,
            )
            .await
            .unwrap();
            for entry in [&windowed, &capped] {
                let outcome = restore_record(&service, &entry.record_id, true)
                    .await
                    .unwrap();
                assert!(outcome.restored, "{} row was evicted again", entry.reason);
            }
        })
        .await;
        let rows = user_memory_lanes::load_lane(service.storage(), Lane::ChatEvidence)
            .await
            .unwrap();
        for key in [
            "chat:self:2026-01-05".to_string(),
            format!("chat:self:{recent}"),
        ] {
            let row = rows
                .iter()
                .find(|row| row["key"] == key.as_str())
                .unwrap_or_else(|| panic!("{key} missing"));
            assert!(row.get(RESTORED_MARKER).is_some());
        }
        // The cap still bounds the unrestored rows.
        assert_eq!(
            rows.iter()
                .filter(|row| row.get(RESTORED_MARKER).is_none())
                .count(),
            2
        );
    }
}
