//! The owner's preference history (`intention_history.v1`).
//!
//! Every change to an intention-bearing knowledge record is journaled beside
//! `knowledge.json` as one JSON line: when, which record, what happened, who
//! wrote it ([`super::actor`]), and the revision and wording before and
//! after. The journal is append-only with one exception: forgetting a record
//! erases its wording from every entry, keeping the dated entries
//! (`wording_erased`). Forget means forgotten.
//!
//! Knowledge changes are journaled by `persist_user_knowledge` (and the
//! `forget_memory` path) under the `knowledge.json` flock, by diffing the
//! document before and after ([`diff_intentions`]); so every writer is
//! captured, not only the owner's surfaces. Policy changes, taste decisions
//! and analyst wording grants are journaled with [`append_event`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::actor::WriteActor;
use super::admission::append_line;
use super::sources::native::{bears_intention, intention_id, render_statement};
use crate::magician_v2::agents::memory_lifecycle::digest;
use crate::magician_v2::agents::storage::AgentStorage;

pub const HISTORY_SCHEMA_VERSION: &str = "intention_history.v1";
pub const HISTORY_FILE: &str = "intention_history.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOp {
    Added,
    Edited,
    Confirmed,
    Superseded,
    Retired,
    Restored,
    ScopeChanged,
    Forgotten,
    PolicyChanged,
    TasteDecided,
    WordingGranted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub schema_version: String,
    pub at_ms: i64,
    pub record_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    pub op: HistoryOp,
    pub actor: WriteActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wording_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wording_after: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wording_erased: bool,
}

/// One record-level change, before the actor and time are stamped.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryChange {
    pub record_id: String,
    pub tier: String,
    pub op: HistoryOp,
    pub revision_before: Option<String>,
    pub revision_after: Option<String>,
    pub wording_before: Option<String>,
    pub wording_after: Option<String>,
}

/// Every intention-bearing record, by intention id.
fn intention_records(knowledge: &Value) -> BTreeMap<String, (String, Value)> {
    let mut records = BTreeMap::new();
    let Some(tiers) = knowledge.as_object() else {
        return records;
    };
    for (tier, items) in tiers {
        let Some(items) = items.as_array() else {
            continue;
        };
        for item in items {
            if bears_intention(tier, item) {
                records.insert(intention_id(tier, item), (tier.clone(), item.clone()));
            }
        }
    }
    records
}

fn lifecycle(item: &Value) -> &str {
    item.get("memory_lifecycle")
        .and_then(Value::as_str)
        .unwrap_or("active")
}

/// What changed in one record, or `None` for bookkeeping only (timestamps,
/// evidence lists, index stamps): those are not history.
fn op_for(before: &Value, after: &Value) -> Option<HistoryOp> {
    if before == after {
        return None;
    }
    let (was, now) = (lifecycle(before), lifecycle(after));
    if was != now {
        return Some(match now {
            "superseded" | "replaced" => HistoryOp::Superseded,
            "active" | "pending_review" | "unresolved" => HistoryOp::Restored,
            _ => HistoryOp::Retired,
        });
    }
    if render_statement(before) != render_statement(after) {
        return Some(HistoryOp::Edited);
    }
    let confirmed =
        |item: &Value| item.get("source_type").and_then(Value::as_str) == Some("owner_confirmed");
    if !confirmed(before) && confirmed(after) {
        return Some(HistoryOp::Confirmed);
    }
    if before.get("scope") != after.get("scope")
        || before.get("memory_context") != after.get("memory_context")
    {
        return Some(HistoryOp::ScopeChanged);
    }
    None
}

/// The intention-level changes from `before` to `after`. A record that
/// disappears is `Forgotten` and carries no wording.
pub fn diff_intentions(before: &Value, after: &Value) -> Vec<HistoryChange> {
    let (old, new) = (intention_records(before), intention_records(after));
    let mut changes = Vec::new();
    for (id, (tier, item)) in &new {
        match old.get(id) {
            None => changes.push(HistoryChange {
                record_id: id.clone(),
                tier: tier.clone(),
                op: HistoryOp::Added,
                revision_before: None,
                revision_after: Some(digest(item)),
                wording_before: None,
                wording_after: Some(render_statement(item)),
            }),
            Some((_, was)) => {
                if let Some(op) = op_for(was, item) {
                    changes.push(HistoryChange {
                        record_id: id.clone(),
                        tier: tier.clone(),
                        op,
                        revision_before: Some(digest(was)),
                        revision_after: Some(digest(item)),
                        wording_before: Some(render_statement(was)),
                        wording_after: Some(render_statement(item)),
                    });
                }
            },
        }
    }
    for (id, (tier, was)) in &old {
        if !new.contains_key(id) {
            changes.push(HistoryChange {
                record_id: id.clone(),
                tier: tier.clone(),
                op: HistoryOp::Forgotten,
                revision_before: Some(digest(was)),
                revision_after: None,
                wording_before: None,
                wording_after: None,
            });
        }
    }
    changes
}

pub fn history_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(HISTORY_FILE)
}

fn rotated(path: &Path) -> PathBuf {
    path.with_extension("jsonl.1")
}

fn entry(change: &HistoryChange, actor: &WriteActor, at_ms: i64) -> HistoryEntry {
    HistoryEntry {
        schema_version: HISTORY_SCHEMA_VERSION.to_string(),
        at_ms,
        record_id: change.record_id.clone(),
        tier: Some(change.tier.clone()),
        op: change.op,
        actor: actor.clone(),
        revision_before: change.revision_before.clone(),
        revision_after: change.revision_after.clone(),
        wording_before: change.wording_before.clone(),
        wording_after: change.wording_after.clone(),
        wording_erased: false,
    }
}

/// Journal knowledge changes. The caller holds the `knowledge.json`
/// exclusive flock, which also serializes [`append_event`] and erasure.
pub async fn append_changes_locked(
    storage: &AgentStorage,
    changes: &[HistoryChange],
    actor: &WriteActor,
    at_ms: i64,
) -> std::io::Result<()> {
    let path = history_path(storage);
    for change in changes {
        let line =
            serde_json::to_value(entry(change, actor, at_ms)).map_err(std::io::Error::other)?;
        append_line(&path, &line).await?;
    }
    let forgotten: BTreeSet<&str> = changes
        .iter()
        .filter(|change| change.op == HistoryOp::Forgotten)
        .map(|change| change.record_id.as_str())
        .collect();
    if !forgotten.is_empty() {
        erase_wording(&rotated(&path), &forgotten).await?;
        erase_wording(&path, &forgotten).await?;
    }
    Ok(())
}

/// Journal an event that is not a knowledge write: a policy change, a taste
/// decision, an analyst wording grant. Takes the `knowledge.json` flock to
/// serialize with erasure, so it must not be called while holding it.
pub async fn append_event(storage: &AgentStorage, event: HistoryEntry) -> std::io::Result<()> {
    let _flock = AgentStorage::acquire_file_lock_exclusive(&storage.user_knowledge_path())
        .await
        .map_err(std::io::Error::other)?;
    let line = serde_json::to_value(&event).map_err(std::io::Error::other)?;
    append_line(&history_path(storage), &line).await
}

/// A non-knowledge event with this module's schema version.
pub fn event(record_id: &str, op: HistoryOp, actor: WriteActor, at_ms: i64) -> HistoryEntry {
    HistoryEntry {
        schema_version: HISTORY_SCHEMA_VERSION.to_string(),
        at_ms,
        record_id: record_id.to_string(),
        tier: None,
        op,
        actor,
        revision_before: None,
        revision_after: None,
        wording_before: None,
        wording_after: None,
        wording_erased: false,
    }
}

/// Journal the owner's decision on a taste-profile proposal, under the
/// profile line's stable id (`sources::taste_profile::line_id`).
pub async fn record_taste_decision(
    storage: &AgentStorage,
    directive: &str,
    decision: &str,
    proposal_id: &str,
    surface: Option<&str>,
    at_ms: i64,
) -> std::io::Result<()> {
    let actor = surface
        .map(WriteActor::surface)
        .unwrap_or_else(|| WriteActor::via("taste_review"))
        .with_cause(format!("{decision} taste proposal {proposal_id}"));
    let mut entry = event(
        &super::sources::taste_profile::line_id(directive),
        HistoryOp::TasteDecided,
        actor,
        at_ms,
    );
    entry.tier = Some("taste_profile".to_string());
    entry.wording_after = Some(directive.to_string());
    append_event(storage, entry).await
}

/// Erase the wording of every entry about `records` in one journal file,
/// rewriting it atomically. Unparseable lines are kept as they are.
async fn erase_wording(path: &Path, records: &BTreeSet<&str>) -> std::io::Result<()> {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<HistoryEntry>(line) {
            Ok(mut entry) if records.contains(entry.record_id.as_str()) => {
                if entry.wording_before.is_some()
                    || entry.wording_after.is_some()
                    || !entry.wording_erased
                {
                    entry.wording_before = None;
                    entry.wording_after = None;
                    entry.wording_erased = true;
                    changed = true;
                }
                out.push_str(&serde_json::to_string(&entry).map_err(std::io::Error::other)?);
            },
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    if !changed {
        return Ok(());
    }
    magician_core::durable_io::write_bytes_durably(path, out.as_bytes()).await
}

async fn read_entries(path: &Path) -> std::io::Result<Vec<HistoryEntry>> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// History, newest first, optionally for one record, paged.
pub async fn read_history(
    storage: &AgentStorage,
    record: Option<&str>,
    offset: usize,
    limit: usize,
) -> std::io::Result<Vec<HistoryEntry>> {
    let path = history_path(storage);
    let mut entries = read_entries(&rotated(&path)).await?;
    entries.extend(read_entries(&path).await?);
    Ok(entries
        .into_iter()
        .rev()
        .filter(|entry| record.map_or(true, |id| entry.record_id == id))
        .skip(offset)
        .take(limit)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::agents::AgentMemoryService;
    use serde_json::json;

    fn doc(items: Value) -> Value {
        json!({ "preferences": items })
    }

    #[test]
    fn an_added_record_is_added_and_a_removed_one_is_forgotten_without_wording() {
        let before = doc(json!([]));
        let after = doc(json!([{"memory_record_id": "a", "key": "dinner", "value": "No meat"}]));
        let changes = diff_intentions(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].op, HistoryOp::Added);
        assert_eq!(changes[0].wording_after.as_deref(), Some("dinner: No meat"));
        let gone = diff_intentions(&after, &before);
        assert_eq!(gone[0].op, HistoryOp::Forgotten);
        assert_eq!(gone[0].wording_before, None);
    }

    #[test]
    fn edits_confirmations_and_lifecycle_changes_are_named() {
        let base = json!({"memory_record_id": "a", "key": "dinner", "value": "No meat"});
        let mut edited = base.clone();
        edited["value"] = json!("No red meat");
        let mut confirmed = base.clone();
        confirmed["source_type"] = json!("owner_confirmed");
        let mut superseded = base.clone();
        superseded["memory_lifecycle"] = json!("superseded");
        superseded["superseded_by"] = json!("b");
        let mut retracted = base.clone();
        retracted["memory_lifecycle"] = json!("retracted");
        let mut scoped = base.clone();
        scoped["memory_context"] = json!("when ordering for guests");
        let mut stamped = base.clone();
        stamped["updated_at"] = json!("2026-10-10T00:00:00Z");
        let op = |after: Value| {
            diff_intentions(&doc(json!([base.clone()])), &doc(json!([after])))
                .first()
                .map(|change| change.op)
        };
        assert_eq!(op(edited), Some(HistoryOp::Edited));
        assert_eq!(op(confirmed), Some(HistoryOp::Confirmed));
        assert_eq!(op(superseded), Some(HistoryOp::Superseded));
        assert_eq!(op(retracted), Some(HistoryOp::Retired));
        assert_eq!(op(scoped), Some(HistoryOp::ScopeChanged));
        assert_eq!(op(stamped), None);
    }

    #[test]
    fn records_that_are_not_intentions_are_not_history() {
        let before = json!({
            "contacts": [{"key": "x", "value": "y"}],
            "preferences": [{"memory_record_id": "a", "key": "k", "value": "v"},
                            {"memory_record_id": "f", "key": "k", "value": "a fact", "memory_kind": "fact"}],
            "_memory_lifecycle": [{"key": "journal"}]
        });
        let mut after = before.clone();
        after["contacts"][0]["value"] = json!("z");
        after["preferences"][1]["value"] = json!("another fact");
        after["_memory_lifecycle"][0]["key"] = json!("changed");
        assert!(diff_intentions(&before, &after).is_empty());
    }

    fn change(op: HistoryOp, wording: Option<&str>) -> HistoryChange {
        HistoryChange {
            record_id: "a".to_string(),
            tier: "preferences".to_string(),
            op,
            revision_before: None,
            revision_after: None,
            wording_before: None,
            wording_after: wording.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn forgetting_erases_earlier_wording_and_keeps_the_dated_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let storage = service.storage();
        let web = WriteActor::surface("web");
        append_changes_locked(
            storage,
            &[change(HistoryOp::Added, Some("dinner: No meat"))],
            &web,
            1,
        )
        .await
        .unwrap();
        let mut other = change(HistoryOp::Added, Some("coffee: decaf"));
        other.record_id = "b".to_string();
        append_changes_locked(storage, &[other], &web, 2)
            .await
            .unwrap();
        append_changes_locked(storage, &[change(HistoryOp::Forgotten, None)], &web, 3)
            .await
            .unwrap();

        let about_a = read_history(storage, Some("a"), 0, 50).await.unwrap();
        assert_eq!(about_a.len(), 2);
        assert_eq!(about_a[0].op, HistoryOp::Forgotten);
        assert!(about_a
            .iter()
            .all(|entry| entry.wording_erased && entry.wording_after.is_none()));
        let raw = tokio::fs::read_to_string(history_path(storage))
            .await
            .unwrap();
        assert!(!raw.contains("No meat"));
        // Other records keep their wording.
        let about_b = read_history(storage, Some("b"), 0, 50).await.unwrap();
        assert_eq!(about_b[0].wording_after.as_deref(), Some("coffee: decaf"));
    }

    #[tokio::test]
    async fn history_reads_newest_first_and_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let storage = service.storage();
        for at in 1..=5 {
            append_event(
                storage,
                event(
                    "policy",
                    HistoryOp::PolicyChanged,
                    WriteActor::surface("ios"),
                    at,
                ),
            )
            .await
            .unwrap();
        }
        let page = read_history(storage, None, 1, 2).await.unwrap();
        assert_eq!(
            page.iter().map(|entry| entry.at_ms).collect::<Vec<_>>(),
            [4, 3]
        );
    }

    #[tokio::test]
    async fn every_knowledge_write_lands_in_the_history_with_its_writer() {
        use super::super::actor::with_actor;
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .update_user_knowledge_as("memory_tool", |knowledge| {
                *knowledge = json!({"preferences": [
                    {"memory_record_id": "a", "key": "dinner", "value": "No meat"}
                ]});
                Ok(true)
            })
            .await
            .unwrap();
        // The owner surface names the writer; the helper's default yields.
        with_actor(
            WriteActor::surface("web"),
            service.update_user_knowledge_as("memory_api", |knowledge| {
                knowledge["preferences"][0]["value"] = json!("No red meat");
                Ok(true)
            }),
        )
        .await
        .unwrap();
        let entries = read_history(service.storage(), Some("a"), 0, 10)
            .await
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].op, HistoryOp::Edited);
        assert_eq!(entries[0].actor.surface.as_deref(), Some("web"));
        assert_eq!(
            entries[0].wording_before.as_deref(),
            Some("dinner: No meat")
        );
        assert_eq!(entries[1].op, HistoryOp::Added);
        assert_eq!(entries[1].actor.via, "memory_tool");
    }

    #[tokio::test]
    async fn policy_changes_and_taste_decisions_are_journaled() {
        use crate::magician_v2::agents::AgentMemoryResolver;
        use crate::magician_v2::preference_alignment::intention::InterventionPolicy;
        use crate::magician_v2::preference_alignment::policy::{write_policy, PolicyChange};
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let change = PolicyChange {
            expected_revision: 0,
            global: Some(InterventionPolicy::Advisory),
            intentions: Default::default(),
        };
        write_policy(&resolver, "owner", "default", change, false, Some("ios"), 7)
            .await
            .unwrap();
        let memory = resolver.resolve_for_scope("owner", "default").unwrap();
        record_taste_decision(
            memory.storage(),
            "- Prefer window seats",
            "approved",
            "p1",
            Some("web"),
            8,
        )
        .await
        .unwrap();
        let entries = read_history(memory.storage(), None, 0, 10).await.unwrap();
        assert_eq!(entries[0].op, HistoryOp::TasteDecided);
        assert!(entries[0].record_id.starts_with("taste:"));
        assert_eq!(entries[1].op, HistoryOp::PolicyChanged);
        assert_eq!(entries[1].revision_after.as_deref(), Some("r1"));
        assert_eq!(entries[1].actor.surface.as_deref(), Some("ios"));
    }
}
