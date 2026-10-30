//! Size bounds over a user-knowledge document: agent-made restatements
//! stored once, a cap per section, a cap on records waiting for review.
//!
//! Pure and two-phase. [`plan`] reads the document and says what would leave
//! it (with the records themselves, ready for the archive); [`apply`] makes
//! that change. Callers archive the plan's records *before* applying it, so a
//! failed archive write leaves the document untouched. Observe mode plans and
//! logs; only enforce mode applies.
//!
//! Never removed, by any rule here: owner-stated (`Stated` trust),
//! owner-confirmed or corrected records, records the owner asked for through
//! the learning path, records the reviewer made active, records restored from
//! the archive, app-governed records, and records in — or related to — an
//! open owner question (removing a related record would make the question's
//! evidence stale and dismiss it). The one exception is an exact duplicate:
//! two records with the same applicability and the same wording are one
//! claim, so the weaker copy is merged into the stronger (its evidence
//! carried, its wording archived). Restored, app-governed, unresolved and
//! conflict-related records are never merged either.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::*;
use crate::magician_v2::agents::memory_provenance::MemoryTrust;
use crate::magician_v2::agents::user_memory_bounds::archive::ArchiveEntry;
use crate::magician_v2::agents::user_memory_bounds::settings::MagicianMemoryBoundsSettings;

/// Evidence entries a folded survivor keeps from the wordings it replaced.
/// Their quotes are dropped (each wording is in the archive); ids, times and
/// source types are kept so independent-observation counts survive.
pub const MAX_CARRIED_EVIDENCE: usize = 32;

/// Identical wording under different keys is one claim only when it has at
/// least this many distinct words. Short claims ("yes", a single name) say
/// too little; under different keys they stay apart.
pub const EXACT_DUPLICATE_MIN_WORDS: usize = 6;

/// How often each memory was used, from the temperature overlay.
#[derive(Debug, Default, Clone)]
pub struct Usage {
    by_item: HashMap<(String, String), UsageSignal>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UsageSignal {
    pub uses: u64,
    pub last_used: Option<DateTime<Utc>>,
}

impl Usage {
    /// Aggregate overlay entries for `knowledge.<section>` candidates by item
    /// key, across agents and goals.
    pub fn from_overlay(
        overlay: &crate::magician_v2::agents::memory_temperature::MemoryTemperatureOverlay,
    ) -> Self {
        use crate::magician_v2::agents::memory_temperature::parse_memory_temperature_candidate_key;
        let mut by_item: HashMap<(String, String), UsageSignal> = HashMap::new();
        for entry in overlay.entries.values() {
            let Some(parts) = parse_memory_temperature_candidate_key(&entry.memory_candidate_key)
            else {
                continue;
            };
            let Some(section) = parts.tier_name.strip_prefix("knowledge.") else {
                continue;
            };
            let signal = by_item
                .entry((section.to_string(), parts.item_key.to_string()))
                .or_default();
            signal.uses += u64::from(entry.selected_count)
                + u64::from(entry.injected_count)
                + u64::from(entry.successful_use_count)
                + u64::from(entry.reviewed_useful_count)
                + u64::from(entry.reviewed_load_bearing_count);
            let last = [
                entry.last_used_at,
                entry.last_injected_at,
                entry.last_selected_at,
            ]
            .into_iter()
            .flatten()
            .max();
            signal.last_used = signal.last_used.max(last);
        }
        Self { by_item }
    }

    pub fn is_empty(&self) -> bool {
        self.by_item.is_empty()
    }

    fn signal(&self, section: &str, item: &Value) -> UsageSignal {
        crate::magician_v2::agents::memory_candidates::item_memory_key(item)
            .and_then(|key| self.by_item.get(&(section.to_string(), key)).copied())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn insert(&mut self, section: &str, item_key: &str, signal: UsageSignal) {
        self.by_item
            .insert((section.to_string(), item_key.to_string()), signal);
    }
}

/// One record leaving a section.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub section: String,
    pub index: usize,
    pub reason: &'static str,
    pub replaced_by: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Plan {
    pub removals: Vec<Removal>,
    /// `(section, index, evidence)` for fold survivors.
    pub survivor_evidence: Vec<(String, usize, Vec<Value>)>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.removals.is_empty()
    }

    /// Removal counts by `(section, reason)`.
    pub fn counts(&self) -> BTreeMap<(String, &'static str), usize> {
        let mut counts = BTreeMap::new();
        for removal in &self.removals {
            *counts
                .entry((removal.section.clone(), removal.reason))
                .or_default() += 1;
        }
        counts
    }

    /// The removed records as archive entries.
    pub fn archive_entries(&self, document: &Value, now: DateTime<Utc>) -> Vec<ArchiveEntry> {
        self.removals
            .iter()
            .filter_map(|removal| {
                let record = document.get(&removal.section)?.get(removal.index)?;
                Some(ArchiveEntry::new(
                    &removal.section,
                    removal.reason,
                    record.clone(),
                    removal.replaced_by.clone(),
                    now,
                ))
            })
            .collect()
    }
}

/// The top-level sections the bounds govern: registry tiers stored as a
/// top-level array. Nested objects (the legacy `user` object holding the
/// activity logs) are left to the one-time migration.
pub fn sections(document: &Value) -> Vec<String> {
    document
        .as_object()
        .into_iter()
        .flat_map(|map| map.iter())
        .filter(|(name, value)| {
            !name.starts_with('_')
                && value.is_array()
                && crate::magician_v2::agents::user_memory_bounds::is_registered_section(name)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Waiting for the lifecycle reviewer. A record with neither review flag
/// predates the reviewer and counts as reviewed (owner decision 2026-10-18,
/// option A: `memory_review_version: 0`).
pub fn waiting_for_review(item: &Value) -> bool {
    item.get("memory_review_needed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn at_most_inferred(item: &Value) -> bool {
    item.get("source_type")
        .and_then(Value::as_str)
        .is_none_or(|source| MemoryTrust::from_source_type(source) != MemoryTrust::Stated)
}

fn reviewed_active(item: &Value) -> bool {
    item.get("memory_review_version")
        .and_then(Value::as_u64)
        .is_some_and(|version| version >= 1)
        && state(item) == "active"
}

fn owner_asked(item: &Value) -> bool {
    let learning = item.get("learning");
    ["explicit_user_request", "explicit_user_correction"]
        .iter()
        .any(|flag| {
            learning
                .and_then(|l| l.get(*flag))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
}

/// Open owner questions: their anchors (as stored) and the ids they cite.
struct OpenConflicts {
    anchors: Vec<Value>,
    ids: BTreeSet<String>,
}

fn open_conflicts(document: &Value) -> OpenConflicts {
    let mut anchors = Vec::new();
    let mut ids = BTreeSet::new();
    for conflict in document[JOURNAL]["conflicts"]
        .as_object()
        .into_iter()
        .flat_map(|map| map.values())
    {
        if !matches!(
            conflict.get("state").and_then(Value::as_str),
            Some("pending" | "clarifying")
        ) {
            continue;
        }
        if let Some(id) = conflict.get("id").and_then(Value::as_str) {
            ids.insert(id.to_string());
        }
        for side in ["incoming", "existing"] {
            if let Some(id) = conflict[side].get("id").and_then(Value::as_str) {
                ids.insert(id.to_string());
            }
            if conflict[side]["item"].is_object() {
                anchors.push(conflict[side]["item"].clone());
            }
        }
    }
    OpenConflicts { anchors, ids }
}

/// Mirrors the relation `context_revision` uses to pick a question's context.
fn related(item: &Value, anchor: &Value) -> bool {
    ["project_id", "engagement_scope", "scope", "subject"]
        .iter()
        .all(|key| item.get(key) == anchor.get(key))
        && (item.get("key").is_some() && item.get("key") == anchor.get("key")
            || item.get("memory_aspect").is_some()
                && item.get("memory_aspect") == anchor.get("memory_aspect")
                && item.get("memory_subject") == anchor.get("memory_subject"))
}

fn protected(item: &Value, conflicts: &OpenConflicts) -> bool {
    if !item.is_object() {
        return true;
    }
    explicit(item)
        || !at_most_inferred(item)
        || item.get("owner_confirmed_at").is_some()
        || owner_asked(item)
        || reviewed_active(item)
        || state(item) == "unresolved"
        || item.get("resolves_conflict").is_some()
        || item.get("memory_restored_at").is_some()
        || item.get("app_source_eligibility").is_some()
        || item
            .get("memory_conflict_id")
            .and_then(Value::as_str)
            .is_some_and(|id| conflicts.ids.contains(id))
        || item
            .get("memory_record_id")
            .and_then(Value::as_str)
            .is_some_and(|id| conflicts.ids.contains(id))
        || conflicts.anchors.iter().any(|anchor| related(item, anchor))
}

fn saved_at(item: &Value) -> String {
    item.get("memory_saved_at")
        .or_else(|| item.get("updated_at"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn retired_at(item: &Value) -> Option<DateTime<Utc>> {
    item.get("superseded_at")
        .or_else(|| item.get("retired_at"))
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|at| at.with_timezone(&Utc))
}

/// Same key and same applicability: a restatement of one claim. Keys compare
/// by their letters and digits only, so `contextual-writing-output-format`,
/// `contextual_writing_output_format` and `ContextualWritingOutputFormat` are
/// one key (each producer spells the key it makes up its own way).
fn fold_group(item: &Value) -> Option<String> {
    let key = normalized_key(item.get("key")?.as_str()?);
    if key.is_empty() {
        return None;
    }
    Some(json!([key, scope_of(item)]).to_string())
}

pub fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The applicability two records must share to be one claim: where it holds,
/// in what context, and for how long (a time-boxed copy is not the same claim
/// as a standing one).
pub fn scope_of(item: &Value) -> Value {
    json!([
        item.get("project_id"),
        item.get("engagement_scope"),
        item.get("scope"),
        item.get("subject"),
        item.get("memory_context"),
        item.get("valid_from"),
        item.get("valid_until")
    ])
}

/// A claim's distinct words.
fn claim_words(item: &Value) -> BTreeSet<String> {
    text(item)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Groups of `members` restating one key (however it is spelled) under the
/// same applicability. Records that say one thing under different keys are
/// not guessed at here: the lifecycle reviewer judges meaning, and
/// `memory-dedupe-user` proposes such groups for it.
fn restatement_groups(items: &[Value], members: &[usize]) -> Vec<Vec<usize>> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &index in members {
        if let Some(group) = fold_group(&items[index]) {
            groups.entry(group).or_default().push(index);
        }
    }
    groups
        .into_values()
        .filter(|group| group.len() > 1)
        .collect()
}

/// Protected only by who stated it (not by an open question, a restore, an
/// app or an unresolved state): such a record may absorb, or be absorbed by,
/// an exact duplicate of itself.
fn mergeable_when_identical(item: &Value, conflicts: &OpenConflicts) -> bool {
    item.is_object()
        && !retired(item)
        && state(item) != "unresolved"
        && item.get("resolves_conflict").is_none()
        && item.get("memory_restored_at").is_none()
        && item.get("app_source_eligibility").is_none()
        && !item
            .get("memory_conflict_id")
            .and_then(Value::as_str)
            .is_some_and(|id| conflicts.ids.contains(id))
        && !item
            .get("memory_record_id")
            .and_then(Value::as_str)
            .is_some_and(|id| conflicts.ids.contains(id))
        && !conflicts.anchors.iter().any(|anchor| related(item, anchor))
}

/// Which copy of an exact duplicate survives: the owner's strongest word,
/// then the newest.
pub fn duplicate_rank(item: &Value) -> (bool, bool, bool, bool, String) {
    (
        item.get("owner_confirmed_at").is_some(),
        explicit(item) || !at_most_inferred(item),
        owner_asked(item),
        reviewed_active(item),
        saved_at(item),
    )
}

/// Record `members` (all but `survivor`) leaving for `survivor`, which keeps
/// its own evidence first and then the newest carried from the others.
fn fold_into(
    plan: &mut Plan,
    removed: &mut BTreeSet<usize>,
    section: &str,
    items: &[Value],
    survivor: usize,
    members: &[usize],
    reason: &'static str,
) {
    let survivor_id = record_id(&items[survivor]);
    // A record already folded into earlier in this plan brings what it holds.
    let planned = |plan: &mut Plan, index: usize| {
        plan.survivor_evidence
            .iter()
            .position(|(s, at, _)| s == section && *at == index)
            .map(|position| plan.survivor_evidence.remove(position).2)
            .unwrap_or_else(|| evidence(&items[index]))
    };
    let mut carried = planned(plan, survivor);
    let own = carried.len();
    for &index in members.iter().filter(|&&index| index != survivor) {
        let archived_id = record_id(&items[index]);
        carried.extend(planned(plan, index).into_iter().map(|mut entry| {
            if let Some(object) = entry.as_object_mut() {
                object.remove("quote");
                object
                    .entry("archived_record_id")
                    .or_insert_with(|| json!(archived_id));
            }
            entry
        }));
        plan.removals.push(Removal {
            section: section.to_string(),
            index,
            reason,
            replaced_by: Some(survivor_id.clone()),
        });
        removed.insert(index);
    }
    if carried.len() > MAX_CARRIED_EVIDENCE.max(own) {
        let mut rest = carried.split_off(own);
        rest.sort_by_key(|entry| {
            std::cmp::Reverse(
                entry
                    .get("at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        });
        rest.truncate(MAX_CARRIED_EVIDENCE.saturating_sub(own));
        carried.extend(rest);
    }
    plan.survivor_evidence
        .push((section.to_string(), survivor, carried));
}

/// For each record of `section`: whether every removal rule leaves it alone
/// (the owner's own words, confirmations, open questions, ...). For reports.
pub fn protected_flags(document: &Value, section: &str) -> Vec<bool> {
    let conflicts = open_conflicts(document);
    document
        .get(section)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| protected(item, &conflicts))
                .collect()
        })
        .unwrap_or_default()
}

/// Work out what the bounds would remove. Reads only.
pub fn plan(
    document: &Value,
    settings: &MagicianMemoryBoundsSettings,
    usage: Option<&Usage>,
    now: DateTime<Utc>,
) -> Plan {
    let conflicts = open_conflicts(document);
    let mut plan = Plan::default();
    let empty = Usage::default();
    let usage = usage.unwrap_or(&empty);
    for section in sections(document) {
        let Some(items) = document.get(&section).and_then(Value::as_array) else {
            continue;
        };
        let mut removed: BTreeSet<usize> = BTreeSet::new();
        let evictable = |index: usize, item: &Value, removed: &BTreeSet<usize>| {
            !removed.contains(&index)
                && item.is_object()
                && !retired(item)
                && !protected(item, &conflicts)
        };

        // 1. Agent-made restatements of one key, however it is spelled, are
        //    stored once. The newest wording survives.
        // 2. Exact duplicates are stored once, even the owner's own records:
        //    the same applicability and the same wording is one claim. The
        //    owner's strongest copy survives.
        if settings
            .fold_inferred_same_key
            .iter()
            .any(|name| name == &section)
        {
            let agent_made: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(index, item)| evictable(*index, item, &removed))
                .map(|(index, _)| index)
                .collect();
            for members in restatement_groups(items, &agent_made) {
                let survivor = *members
                    .iter()
                    .max_by_key(|&&index| (saved_at(&items[index]), index))
                    .expect("group has members");
                fold_into(
                    &mut plan,
                    &mut removed,
                    &section,
                    items,
                    survivor,
                    &members,
                    "restated_by_producer",
                );
            }

            let mut identical: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (index, item) in items.iter().enumerate() {
                if removed.contains(&index) || !mergeable_when_identical(item, &conflicts) {
                    continue;
                }
                let wording = normalized(&text(item));
                if wording.is_empty() {
                    continue;
                }
                // A short wording is one claim only under the same key too.
                let key = if claim_words(item).len() < EXACT_DUPLICATE_MIN_WORDS {
                    match fold_group(item) {
                        Some(key) => key,
                        None => continue,
                    }
                } else {
                    String::new()
                };
                identical
                    .entry(json!([wording, scope_of(item), key]).to_string())
                    .or_default()
                    .push(index);
            }
            for members in identical.values().filter(|members| members.len() > 1) {
                // Only groups holding an owner record are new here; agent-made
                // copies were folded above.
                if members
                    .iter()
                    .all(|&index| !protected(&items[index], &conflicts))
                {
                    continue;
                }
                let survivor = *members
                    .iter()
                    .max_by_key(|&&index| (duplicate_rank(&items[index]), index))
                    .expect("group has members");
                fold_into(
                    &mut plan,
                    &mut removed,
                    &section,
                    items,
                    survivor,
                    members,
                    "merged_duplicate",
                );
            }
        }

        // Least used first, then oldest.
        let coldest_first = |a: &usize, b: &usize| {
            let (sa, sb) = (
                usage.signal(&section, &items[*a]),
                usage.signal(&section, &items[*b]),
            );
            (sa.uses, sa.last_used, saved_at(&items[*a]), *a).cmp(&(
                sb.uses,
                sb.last_used,
                saved_at(&items[*b]),
                *b,
            ))
        };

        // 2. A cap on records per section.
        if let Some(&cap) = settings.max_live.get(&section) {
            let mut live = items.len() - removed.len();
            if live > cap {
                let horizon =
                    now - chrono::Duration::days(i64::from(settings.retired_archive_after_days));
                let mut old_retired: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(index, item)| {
                        !removed.contains(index)
                            && retired(item)
                            && retired_at(item).is_some_and(|at| at < horizon)
                    })
                    .map(|(index, _)| index)
                    .collect();
                old_retired.sort_by_key(|&index| (retired_at(&items[index]), index));
                let mut inferred: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(index, item)| evictable(*index, item, &removed))
                    .map(|(index, _)| index)
                    .collect();
                inferred.sort_by(|a, b| coldest_first(a, b));
                for index in old_retired.into_iter().chain(inferred) {
                    if live <= cap {
                        break;
                    }
                    plan.removals.push(Removal {
                        section: section.clone(),
                        index,
                        reason: "section_cap",
                        replaced_by: None,
                    });
                    removed.insert(index);
                    live -= 1;
                }
            }
        }

        // 3. A cap on records waiting for review.
        let cap = settings.review.max_pending_per_section;
        let mut waiting: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(index, item)| {
                !removed.contains(index) && !retired(item) && waiting_for_review(item)
            })
            .map(|(index, _)| index)
            .collect();
        if waiting.len() > cap {
            let mut over = waiting.len() - cap;
            waiting.retain(|&index| evictable(index, &items[index], &removed));
            waiting.sort_by(|a, b| coldest_first(a, b));
            for index in waiting {
                if over == 0 {
                    break;
                }
                plan.removals.push(Removal {
                    section: section.clone(),
                    index,
                    reason: "review_queue_cap",
                    replaced_by: None,
                });
                removed.insert(index);
                over -= 1;
            }
        }
    }
    plan
}

/// Apply `plan` to the document it was made from. Fold survivors take the
/// carried evidence; removed records leave their sections; open owner
/// questions follow their records to their new positions.
pub fn apply(document: &mut Value, plan: &Plan) {
    if plan.removals.is_empty() && plan.survivor_evidence.is_empty() {
        return;
    }
    for (section, index, evidence) in &plan.survivor_evidence {
        if let Some(item) = document
            .get_mut(section)
            .and_then(|items| items.get_mut(*index))
        {
            item["memory_evidence"] = json!(evidence);
        }
    }
    let mut by_section: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    for removal in &plan.removals {
        by_section
            .entry(removal.section.as_str())
            .or_default()
            .insert(removal.index);
    }
    // Only a question whose context is current now is carried over; a stale
    // one stays stale, exactly as it would have.
    let carried: Vec<String> = document[JOURNAL]["conflicts"]
        .as_object()
        .into_iter()
        .flat_map(|map| map.iter())
        .filter_map(|(id, value)| {
            let conflict: Conflict = serde_json::from_value(value.clone()).ok()?;
            (matches!(conflict.state.as_str(), "pending" | "clarifying")
                && context_revision(document, &[&conflict.incoming, &conflict.existing])
                    == conflict.context_revision)
                .then(|| id.clone())
        })
        .collect();
    for (section, indices) in &by_section {
        if let Some(items) = document.get_mut(*section).and_then(Value::as_array_mut) {
            let mut index = 0;
            items.retain(|_| {
                let keep = !indices.contains(&index);
                index += 1;
                keep
            });
        }
    }
    for id in carried {
        let Ok(mut conflict) =
            serde_json::from_value::<Conflict>(document[JOURNAL]["conflicts"][&id].clone())
        else {
            continue;
        };
        for source in [&mut conflict.incoming, &mut conflict.existing] {
            source.pointer = remapped_pointer(&source.pointer, &by_section);
        }
        conflict.context_revision =
            context_revision(document, &[&conflict.incoming, &conflict.existing]);
        document[JOURNAL]["conflicts"][&id] = json!(conflict);
    }
}

fn remapped_pointer(pointer: &str, removed: &BTreeMap<&str, BTreeSet<usize>>) -> String {
    let mut parts = pointer.trim_start_matches('/').splitn(2, '/');
    let (Some(section), Some(index)) = (parts.next(), parts.next()) else {
        return pointer.to_string();
    };
    let section_name = section.replace("~1", "/").replace("~0", "~");
    let (Some(indices), Ok(index)) = (removed.get(section_name.as_str()), index.parse::<usize>())
    else {
        return pointer.to_string();
    };
    let shift = indices.range(..index).count();
    format!("/{section}/{}", index - shift)
}

/// Stamp `memory_review_version: 0` on records that predate the reviewer
/// (neither review flag), so they count as reviewed (option A). Returns how
/// many were stamped.
pub fn stamp_pre_review_records(document: &mut Value) -> usize {
    let mut stamped = 0;
    for section in sections(document) {
        if let Some(items) = document.get_mut(&section).and_then(Value::as_array_mut) {
            for item in items.iter_mut().filter(|item| item.is_object()) {
                if item.get("memory_review_needed").is_none()
                    && item.get("memory_review_version").is_none()
                {
                    item["memory_review_version"] = json!(0);
                    stamped += 1;
                }
            }
        }
    }
    stamped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn wording(key: &str, index: usize, source_type: &str) -> Value {
        let mut item = json!({"key":key,"value":format!("Wording number {index} of the step"),
            "source_type":source_type,
            "updated_at":format!("2026-09-{:02}T08:00:00+00:00", 1 + index % 28)});
        let ingested = merge_collection(&[], &[item.clone()], "workflows", now());
        item = ingested[0].clone();
        item["memory_saved_at"] = json!(format!(
            "2026-{:02}-{:02}T08:00:00+00:00",
            8 + index / 28,
            1 + index % 28
        ));
        item
    }

    fn settings() -> MagicianMemoryBoundsSettings {
        MagicianMemoryBoundsSettings::default()
    }

    fn run(document: &mut Value, usage: Option<&Usage>) -> Plan {
        let plan = plan(document, &settings(), usage, now());
        apply(document, &plan);
        plan
    }

    #[test]
    fn seventy_six_rewordings_leave_one_live_record_and_seventy_five_archived() {
        let items: Vec<Value> = (0..76)
            .map(|i| wording("weekly-report", i, "inferred"))
            .collect();
        let newest = record_id(&items[75]);
        let mut document = json!({ "workflows": items });
        let before = document.clone();
        let plan = run(&mut document, None);
        let entries = plan.archive_entries(&before, now());
        assert_eq!(entries.len(), 75);
        assert!(entries.iter().all(|e| e.reason == "restated_by_producer"
            && e.replaced_by.as_deref() == Some(newest.as_str())));
        let live = document["workflows"].as_array().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(record_id(&live[0]), newest);
        // Evidence ids from the replaced wordings are carried, without quotes.
        let evidence = live[0]["memory_evidence"].as_array().unwrap();
        assert!(evidence.len() <= MAX_CARRIED_EVIDENCE && evidence.len() > 1);
        assert!(evidence[1..].iter().all(|e| e.get("quote").is_none()));
        // Re-running finds nothing more to do.
        assert!(super::plan(&document, &settings(), None, now()).is_empty());
    }

    #[test]
    fn owner_words_confirmations_and_conflicts_are_never_folded() {
        let stated = wording("tone", 1, "explicit_user_statement");
        let mut confirmed = wording("tone", 2, "inferred");
        confirmed["owner_confirmed_at"] = json!(now().to_rfc3339());
        let inferred = wording("tone", 3, "inferred");
        let mut conflicted = wording("cadence", 4, "inferred");
        conflicted["memory_lifecycle"] = json!("unresolved");
        let cadence = wording("cadence", 5, "inferred");
        let mut document = json!({"workflows":[stated, confirmed, inferred, conflicted, cadence]});
        let plan = run(&mut document, None);
        assert!(plan.removals.is_empty(), "{plan:?}");
        assert_eq!(document["workflows"].as_array().unwrap().len(), 5);
    }

    fn claim(key: &str, value: &str, source_type: &str, day: usize) -> Value {
        let item = json!({"key":key,"value":value,"source_type":source_type,
            "updated_at":format!("2026-09-{day:02}T08:00:00+00:00")});
        let mut item = merge_collection(&[], &[item], "preferences", now())[0].clone();
        item["memory_saved_at"] = json!(format!("2026-09-{day:02}T08:00:00+00:00"));
        item
    }

    const DRAFT_ONLY: &str = "Return only the draft text; omit explanations, fences and labels.";

    #[test]
    fn agent_made_preferences_fold_when_the_key_is_spelled_differently() {
        let items = vec![
            claim(
                "contextual-writing-output-format",
                "Draft only, no labels",
                "inferred",
                1,
            ),
            claim(
                "contextual_writing_output_format",
                "Only the draft, no labels",
                "inferred",
                2,
            ),
            claim(
                "ContextualWritingOutputFormat",
                "Just the draft text",
                "inferred",
                3,
            ),
            claim("units", "Metric units", "inferred", 4),
        ];
        let newest = record_id(&items[2]);
        let mut document = json!({ "preferences": items });
        let plan = run(&mut document, None);
        assert_eq!(plan.removals.len(), 2, "{plan:?}");
        assert!(plan
            .removals
            .iter()
            .all(|r| r.reason == "restated_by_producer"
                && r.replaced_by.as_deref() == Some(newest.as_str())));
        assert_eq!(document["preferences"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn different_keys_never_fold_on_wording_alone() {
        // One changed word in a long sentence is a different claim; similar
        // wording under other keys is the reviewer's call, not a word count's.
        let long = "Schedule the weekly design sync with the product team on Tuesday mornings";
        let items = vec![
            claim("design_sync_day", long, "inferred", 1),
            claim(
                "weekly_sync",
                &long.replace("Tuesday", "Thursday"),
                "inferred",
                2,
            ),
            claim("response_format_draft_only", DRAFT_ONLY, "inferred", 3),
            claim(
                "draft_text_style",
                &DRAFT_ONLY.to_uppercase().replace(';', ","),
                "inferred",
                4,
            ),
            claim("ack_a", "Yes", "inferred", 5),
            claim("ack_b", "Yes", "inferred", 6),
        ];
        let mut document = json!({ "preferences": items });
        assert!(run(&mut document, None).removals.is_empty());
        assert_eq!(document["preferences"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn a_time_boxed_copy_is_never_the_same_claim_as_a_standing_one() {
        let mut standing = claim("draft_only", DRAFT_ONLY, "explicit_user_statement", 1);
        standing["learning"] = json!({"explicit_user_request": true});
        let mut boxed = claim("draft_only", DRAFT_ONLY, "explicit_user_statement", 2);
        boxed["valid_until"] = json!("2026-11-01T00:00:00Z");
        let mut document = json!({ "preferences": [standing, boxed] });
        assert!(run(&mut document, None).removals.is_empty());
    }

    #[test]
    fn an_agent_rewording_never_replaces_an_owner_preference() {
        let items = vec![
            claim(
                "units",
                "Use metric units for every measurement.",
                "explicit_user_statement",
                1,
            ),
            claim(
                "units",
                "Prefer imperial units in casual messages.",
                "inferred",
                2,
            ),
        ];
        let mut document = json!({ "preferences": items });
        assert!(run(&mut document, None).removals.is_empty());
        assert_eq!(document["preferences"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn exact_duplicates_of_owner_records_merge_into_the_strongest_copy() {
        let stated = claim("draft_only", DRAFT_ONLY, "explicit_user_statement", 1);
        let mut confirmed = claim("draft-format", DRAFT_ONLY, "inferred", 2);
        confirmed["owner_confirmed_at"] = json!(now().to_rfc3339());
        let mut asked = claim("writing_output", DRAFT_ONLY, "inferred", 3);
        asked["learning"] = json!({"explicit_user_request": true});
        let mut restored = claim("restored_copy", DRAFT_ONLY, "inferred", 4);
        restored["memory_restored_at"] = json!(now().to_rfc3339());
        let survivor = record_id(&confirmed);
        let mut document = json!({ "preferences": [stated, confirmed, asked, restored] });
        let before = document.clone();
        let plan = run(&mut document, None);
        let entries = plan.archive_entries(&before, now());
        assert_eq!(entries.len(), 2, "{plan:?}");
        assert!(entries.iter().all(|e| e.reason == "merged_duplicate"
            && e.replaced_by.as_deref() == Some(survivor.as_str())));
        let live = document["preferences"].as_array().unwrap();
        assert_eq!(live.len(), 2, "the restored copy is never merged");
        let kept = live
            .iter()
            .find(|item| record_id(item) == survivor)
            .unwrap();
        assert!(kept["memory_evidence"].as_array().unwrap().len() >= 3);
        assert!(super::plan(&document, &settings(), None, now()).is_empty());
    }

    #[test]
    fn a_section_cap_archives_the_coldest_unprotected_records_and_never_protected_ones() {
        let mut settings = settings();
        settings.max_live.insert("workflows".into(), 3);
        settings.fold_inferred_same_key.clear();
        let mut items: Vec<Value> = (0..5)
            .map(|i| wording(&format!("step-{i}"), i, "inferred"))
            .collect();
        items.push(wording("rule", 9, "explicit_user_statement"));
        items.push(wording("asked", 10, "learning_candidate"));
        items[6]["learning"] = json!({"explicit_user_request": true});
        let mut usage = Usage::default();
        // step-0 is old but used; step-1 and step-2 are unused and oldest.
        usage.insert(
            "workflows",
            &format!("record:{}", items[0]["memory_record_id"].as_str().unwrap()),
            UsageSignal {
                uses: 9,
                last_used: Some(now()),
            },
        );
        let document = json!({ "workflows": items });
        let plan = plan(&document, &settings, Some(&usage), now());
        let removed: Vec<_> = plan
            .removals
            .iter()
            .map(|r| document["workflows"][r.index]["key"].as_str().unwrap())
            .collect();
        assert_eq!(removed, vec!["step-1", "step-2", "step-3", "step-4"]);
        assert!(plan.removals.iter().all(|r| r.reason == "section_cap"));
    }

    #[test]
    fn without_usage_the_cap_falls_back_to_oldest_first() {
        let mut settings = settings();
        settings.max_live.insert("organization".into(), 2);
        let items: Vec<Value> = (0..4)
            .map(|i| wording(&format!("team-{i}"), i, "inferred"))
            .collect();
        let document = json!({ "organization": items });
        let plan = plan(&document, &settings, None, now());
        let removed: Vec<_> = plan.removals.iter().map(|r| r.index).collect();
        assert_eq!(removed, vec![0, 1]);
    }

    #[test]
    fn under_cap_pressure_old_retired_records_go_first() {
        let mut settings = settings();
        settings.max_live.insert("workflows".into(), 2);
        settings.fold_inferred_same_key.clear();
        let mut old = wording("old", 1, "inferred");
        old["memory_lifecycle"] = json!("superseded");
        old["superseded_at"] = json!("2026-08-01T00:00:00Z");
        let mut recent = wording("recent", 2, "inferred");
        recent["memory_lifecycle"] = json!("superseded");
        recent["superseded_at"] = json!("2026-10-17T00:00:00Z");
        let a = wording("a", 3, "inferred");
        let document = json!({"workflows":[old, recent, a]});
        let plan = plan(&document, &settings, None, now());
        assert_eq!(plan.removals.len(), 1);
        assert_eq!(plan.removals[0].index, 0);
    }

    #[test]
    fn the_review_queue_cap_archives_the_least_used_waiting_records() {
        let mut settings = settings();
        settings.review.max_pending_per_section = 2;
        settings.fold_inferred_same_key.clear();
        let items: Vec<Value> = (0..4)
            .map(|i| wording(&format!("pending-{i}"), i, "inferred"))
            .collect();
        assert!(items.iter().all(waiting_for_review));
        let document = json!({ "organization": items });
        let plan = plan(&document, &settings, None, now());
        assert_eq!(plan.removals.len(), 2);
        assert!(plan.removals.iter().all(|r| r.reason == "review_queue_cap"));
    }

    #[test]
    fn an_open_question_keeps_its_records_and_follows_them_to_new_positions() {
        let mut settings = settings();
        settings.max_live.insert("workflows".into(), 3);
        settings.fold_inferred_same_key.clear();
        let mut items: Vec<Value> = (0..5)
            .map(|i| wording(&format!("step-{i}"), i, "inferred"))
            .collect();
        items[3]["memory_lifecycle"] = json!("unresolved");
        items[4]["memory_lifecycle"] = json!("unresolved");
        let mut document = json!({ "workflows": items });
        let all = sources(&document);
        let incoming = all
            .iter()
            .find(|s| s.pointer == "/workflows/4")
            .unwrap()
            .clone();
        let existing = all
            .iter()
            .find(|s| s.pointer == "/workflows/3")
            .unwrap()
            .clone();
        let conflict = Conflict {
            id: "q1".into(),
            incoming: incoming.clone(),
            existing: existing.clone(),
            question: "Which applies?".into(),
            reason: "differs".into(),
            state: "pending".into(),
            created_at: now().timestamp(),
            context_revision: context_revision(&document, &[&incoming, &existing]),
        };
        document[JOURNAL]["conflicts"]["q1"] = json!(conflict);
        let plan = plan(&document, &settings, None, now());
        assert_eq!(plan.removals.len(), 2);
        apply(&mut document, &plan);
        let moved: Conflict =
            serde_json::from_value(document[JOURNAL]["conflicts"]["q1"].clone()).unwrap();
        assert_eq!(moved.incoming.pointer, "/workflows/2");
        assert_eq!(moved.existing.pointer, "/workflows/1");
        assert!(current(&document, &moved.incoming));
        assert!(current(&document, &moved.existing));
        assert_eq!(
            context_revision(&document, &[&moved.incoming, &moved.existing]),
            moved.context_revision
        );
        // The owner can still answer it.
        assert!(answer_conflict(&mut document, "q1", "keep_existing", None, now()).unwrap());
    }

    #[test]
    fn pre_review_records_are_stamped_reviewed_once() {
        let mut document = json!({"workflows":[{"key":"old","value":"An old step"},
            {"key":"new","value":"x","memory_review_needed":true}]});
        assert_eq!(stamp_pre_review_records(&mut document), 1);
        assert_eq!(document["workflows"][0]["memory_review_version"], 0);
        assert!(!waiting_for_review(&document["workflows"][0]));
        assert_eq!(stamp_pre_review_records(&mut document), 0);
    }
}
