//! Shared, source-preserving memory lifecycle. The model proposes relationships;
//! this module owns identities, evidence, scope, version checks and transitions.
//! Review I/O belongs in `runtime`, never inside a storage lock.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub mod bounds;
pub mod candidates;
mod decision;
pub mod provenance;
mod review_wire;
pub mod runtime;
#[cfg(test)]
mod tests;

pub const OPERATION: &str = "memory_lifecycle_review";
pub const POLICY_VERSION: u32 = 1;
pub const JOURNAL: &str = "_memory_lifecycle";

pub fn digest(value: &Value) -> String {
    blake3::hash(value.to_string().as_bytes())
        .to_hex()
        .to_string()
}

fn normalized(value: &str) -> String {
    // Preserve punctuation and negation. Similarity is not identity.
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn text(item: &Value) -> String {
    if let Some(value) = item.get("value") {
        return value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
    }
    for key in ["text", "insight", "summary", "pattern", "directive"] {
        if let Some(value) = item.get(key).and_then(Value::as_str) {
            return value.to_owned();
        }
    }
    item.to_string()
}

pub fn explicit(item: &Value) -> bool {
    item.get("source_type")
        .and_then(Value::as_str)
        .is_some_and(|source| {
            super::memory_provenance::MemoryTrust::from_source_type(source)
                == super::memory_provenance::MemoryTrust::Stated
        })
}

pub fn state(item: &Value) -> &str {
    item.get("memory_lifecycle")
        .and_then(Value::as_str)
        .unwrap_or("active")
}

pub fn retired(item: &Value) -> bool {
    matches!(
        state(item),
        "superseded" | "replaced" | "expired" | "retracted"
    )
}

/// Stable record identity excludes timestamps, review decisions and counters.
/// Array positions and human-chosen keys alone cannot identify two revisions.
pub fn record_id(item: &Value) -> String {
    item.get("memory_record_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            digest(&json!({
                "key": item.get("key"), "claim": normalized(&text(item)),
                "scope": applicability(item), "source_type":item.get("source_type")
            }))
        })
}

fn applicability(item: &Value) -> Value {
    json!({"project_id":item.get("project_id"),"engagement_scope":item.get("engagement_scope"),
        "scope":item.get("scope"),
        "context":item.get("memory_context"),"valid_from":item.get("valid_from"),
        "valid_until":item.get("valid_until"),"subject":item.get("subject")})
}

pub fn evidence(item: &Value) -> Vec<Value> {
    if let Some(values) = item.get("memory_evidence").and_then(Value::as_array) {
        return values.clone();
    }
    // An ingestion-provided root event wins over a copy/summary identity. With
    // no provenance, identical text is only one observation, however often read.
    let id = ["root_source_id", "source_event_id", "source_id"]
        .iter()
        .find_map(|key| item.get(key).and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            digest(&json!({"claim":normalized(&text(item)),"scope":applicability(item)}))
        });
    vec![
        json!({"id":id,"at":item.get("observed_at").or_else(||item.get("updated_at")),
        "source_type":item.get("source_type"),"quote":text(item)}),
    ]
}

fn evidence_citation_key(item: &Value) -> String {
    // Several exact quotes may support one observation. The producer's root
    // still owns independence; citation identity must also preserve speaker
    // and revision so a correction cannot erase the quote it qualifies.
    json!([
        item.get("id"),
        item.get("source_role"),
        item.get("source_id"),
        item.get("source_revision"),
        item.get("quote")
    ])
    .to_string()
}

fn merge_evidence(target: &mut Value, incoming: &Value) {
    let mut by_citation = BTreeMap::new();
    for item in evidence(target).into_iter().chain(evidence(incoming)) {
        if item.get("id").and_then(Value::as_str).is_some() {
            // Do not move the original observation's date forward on replay.
            by_citation
                .entry(evidence_citation_key(&item))
                .or_insert(item);
        }
    }
    target["memory_evidence"] = json!(by_citation.into_values().collect::<Vec<_>>());
}

pub fn independent_observations(item: &Value) -> usize {
    evidence(item)
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn observation_span_days(item: &Value) -> i64 {
    let mut by_root = BTreeMap::<String, DateTime<chrono::FixedOffset>>::new();
    for source in evidence(item) {
        let Some(id) = source.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(at) = source
            .get("at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        else {
            continue;
        };
        by_root
            .entry(id.to_owned())
            .and_modify(|first| *first = (*first).min(at))
            .or_insert(at);
    }
    let times = by_root.values().collect::<Vec<_>>();
    times
        .iter()
        .max()
        .zip(times.iter().min())
        .map(|(last, first)| (**last - **first).num_days())
        .unwrap_or(0)
}

/// Shared ingress for structured arrays. Preserve old revisions; never use an
/// incoming key as permission to destroy an existing claim. Semantic review
/// makes the pending item current or resolves its relationship to existing ones.
pub fn merge_items(existing: &[Value], incoming: &[Value], now: DateTime<Utc>) -> Vec<Value> {
    let mut items = existing.to_vec();
    for raw in incoming {
        if !raw.is_object() {
            items.push(raw.clone());
            continue;
        }
        let id = record_id(raw);
        if let Some(current) = items.iter_mut().find(|e| {
            !retired(e)
                && (record_id(e) == id
                    || e.get("memory_ingress_id").and_then(Value::as_str) == Some(&id))
        }) {
            let before = independent_observations(current);
            let before_citations = evidence(current).len();
            merge_evidence(current, raw);
            if independent_observations(current) > before
                || evidence(current).len() > before_citations
            {
                current["memory_review_needed"] = json!(true);
            }
            continue;
        }
        // The same claim in other words (a key variant, a rewording) is one
        // memory: its evidence folds into the record already held, which
        // review then weighs, rather than a sibling record being saved.
        if let Some(current) = items
            .iter_mut()
            .find(|e| !retired(e) && near_duplicate_record(e, raw))
        {
            let before = independent_observations(current);
            let before_citations = evidence(current).len();
            merge_evidence(current, raw);
            if independent_observations(current) > before
                || evidence(current).len() > before_citations
            {
                current["memory_review_needed"] = json!(true);
            }
            continue;
        }
        if let Some(old) = items.iter().find(|e| {
            retired(e)
                && (record_id(e) == id
                    || e.get("memory_ingress_id").and_then(Value::as_str) == Some(&id))
        }) {
            let raw_evidence = evidence(raw);
            let old_evidence = evidence(old);
            let repeated_event = raw_evidence
                .iter()
                .all(|new| old_evidence.iter().any(|old| old["id"] == new["id"]));
            let later_statement = raw
                .get("updated_at")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .zip(
                    old.get("superseded_at")
                        .or_else(|| old.get("retired_at"))
                        .and_then(Value::as_str)
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok()),
                )
                .is_some_and(|(new, old)| new > old);
            let named_event = ["root_source_id", "source_event_id", "source_id"]
                .iter()
                .any(|key| raw.get(key).and_then(Value::as_str).is_some());
            if !explicit(raw) || (named_event && repeated_event) || !later_statement {
                continue;
            }
        }
        let mut item = raw.clone();
        item["memory_ingress_id"] = json!(id);
        // Same content can be explicitly restored after retirement. That is a
        // new revision, not a collision with the historical record.
        let id = if items.iter().any(|e| record_id(e) == id) {
            digest(&json!([id, now.to_rfc3339()]))
        } else {
            id
        };
        item["memory_record_id"] = json!(id);
        item["memory_evidence"] = json!(evidence(raw));
        let app_governed = item.get("app_source_eligibility").is_some();
        // This envelope is not a grant: the app retrieval path must re-resolve
        // source revisions and eligibility. Never send these records to this
        // reviewer, or strand them pending behind a deliberately excluded path.
        item["memory_lifecycle"] = json!(if app_governed {
            "active"
        } else {
            "pending_review"
        });
        item["memory_review_needed"] = json!(!app_governed);
        item["memory_saved_at"] = json!(now.to_rfc3339());
        items.push(item);
    }
    items
}

/// The same key/claim in two collections must still have distinct identities
/// until cross-tier semantic consolidation explicitly merges them.
pub fn merge_collection(
    existing: &[Value],
    incoming: &[Value],
    collection: &str,
    now: DateTime<Utc>,
) -> Vec<Value> {
    let as_record = |raw: &Value| {
        if raw.is_object() {
            raw.clone()
        } else {
            json!({"key":format!("{collection}:{}",digest(raw)),"value":raw,"source_type":"inferred"})
        }
    };
    let existing: Vec<_> = existing
        .iter()
        .map(|raw| {
            if raw.is_object() {
                raw.clone()
            } else {
                let mut record = as_record(raw);
                record["memory_record_id"] =
                    json!(digest(&json!([collection, record_id(&record)])));
                record["memory_review_needed"] = json!(true);
                record
            }
        })
        .collect();
    let incoming: Vec<_> = incoming
        .iter()
        .map(|raw| {
            let raw = as_record(raw);
            let mut item = raw.clone();
            if item.is_object() {
                item["memory_record_id"] = json!(digest(&json!([collection, record_id(&raw)])));
            }
            item
        })
        .collect();
    merge_items(&existing, &incoming, now)
}

fn claim_key(item: &Value) -> Option<&str> {
    ["key", "name", "title"]
        .iter()
        .find_map(|field| item.get(*field).and_then(Value::as_str))
}

/// Whether `incoming` states `existing`'s claim in other words: a near
/// duplicate by [`magician_vector_index::memory_similarity`] (normalised key,
/// content-word overlap, no differing negation, qualifier or number) with the
/// same applicability, kind and trust. App-governed records are left to their
/// own path.
pub fn near_duplicate_record(existing: &Value, incoming: &Value) -> bool {
    existing.is_object()
        && incoming.is_object()
        && existing.get("app_source_eligibility").is_none()
        && incoming.get("app_source_eligibility").is_none()
        && applicability(existing) == applicability(incoming)
        && existing.get("memory_kind") == incoming.get("memory_kind")
        && explicit(existing) == explicit(incoming)
        && magician_vector_index::memory_similarity::near_duplicate_claims(
            claim_key(existing),
            &text(existing),
            claim_key(incoming),
            &text(incoming),
        )
}

/// One stored record named in a [`NearDuplicateFold`].
#[derive(Clone, Debug, Serialize)]
pub struct FoldRecord {
    pub pointer: String,
    pub id: String,
    /// The record's content digest when planned; a record changed since is
    /// not folded.
    pub revision: String,
    pub key: Option<String>,
    pub observations: usize,
    pub updated_at: Option<String>,
}

impl FoldRecord {
    fn of(source: &Source) -> Self {
        Self {
            pointer: source.pointer.clone(),
            id: source.id.clone(),
            revision: source.revision.clone(),
            key: claim_key(&source.item).map(str::to_owned),
            observations: independent_observations(&source.item),
            updated_at: source
                .item
                .get("updated_at")
                .or_else(|| source.item.get("memory_saved_at"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }
}

/// Near-duplicate copies already stored in one collection, and the record
/// they fold into: the most-confirmed copy (most independent observations),
/// the newest on a tie.
#[derive(Clone, Debug, Serialize)]
pub struct NearDuplicateFold {
    pub collection: String,
    pub keeper: FoldRecord,
    pub folded: Vec<FoldRecord>,
}

/// The folds a maintenance pass would make in `document`: within each
/// collection, every current record (not retired) that is a
/// [`near_duplicate_record`] of a more-confirmed or newer one. Copies are
/// compared with the keeper only, so a chain of small rewordings cannot drift
/// into a different claim. Read-only: [`apply_near_duplicate_folds`] makes
/// them.
pub fn near_duplicate_folds(document: &Value) -> Vec<NearDuplicateFold> {
    let mut by_collection = BTreeMap::<String, Vec<Source>>::new();
    for source in sources(document) {
        if retired(&source.item) || !source.item.is_object() {
            continue;
        }
        let collection = source
            .pointer
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_owned())
            .unwrap_or_default();
        by_collection.entry(collection).or_default().push(source);
    }
    let mut folds = Vec::new();
    for (collection, mut records) in by_collection {
        records.sort_by(|a, b| {
            independent_observations(&b.item)
                .cmp(&independent_observations(&a.item))
                .then_with(|| {
                    FoldRecord::of(b)
                        .updated_at
                        .cmp(&FoldRecord::of(a).updated_at)
                })
                .then_with(|| a.pointer.cmp(&b.pointer))
        });
        let mut assigned = vec![false; records.len()];
        for keeper in 0..records.len() {
            if assigned[keeper] {
                continue;
            }
            let folded = (keeper + 1..records.len())
                .filter(|&other| {
                    !assigned[other]
                        && near_duplicate_record(&records[keeper].item, &records[other].item)
                })
                .collect::<Vec<_>>();
            if folded.is_empty() {
                continue;
            }
            for &other in &folded {
                assigned[other] = true;
            }
            folds.push(NearDuplicateFold {
                collection: collection.clone(),
                keeper: FoldRecord::of(&records[keeper]),
                folded: folded
                    .iter()
                    .map(|&other| FoldRecord::of(&records[other]))
                    .collect(),
            });
        }
    }
    folds
}

/// Make `folds` through the lifecycle: each copy's evidence merges into its
/// keeper and the copy is superseded by it (`near_duplicate_of_record`), so
/// nothing is deleted and every observation is kept. A record that is no
/// longer where, or as, the plan found it is skipped. Returns how many copies
/// folded.
pub fn apply_near_duplicate_folds(
    document: &mut Value,
    folds: &[NearDuplicateFold],
    now: DateTime<Utc>,
) -> usize {
    let mut count = 0;
    for fold in folds {
        let Some(mut keeper) = document.pointer(&fold.keeper.pointer).cloned() else {
            continue;
        };
        if digest(&keeper) != fold.keeper.revision || retired(&keeper) {
            continue;
        }
        let before = independent_observations(&keeper);
        let mut copies = Vec::new();
        for copy in &fold.folded {
            let Some(mut item) = document.pointer(&copy.pointer).cloned() else {
                continue;
            };
            if digest(&item) != copy.revision || retired(&item) {
                continue;
            }
            merge_evidence(&mut keeper, &item);
            item["memory_record_id"] = json!(copy.id);
            supersede(&mut item, &fold.keeper.id, "near_duplicate_of_record", now);
            item["memory_reconciled_at"] = json!(now.to_rfc3339());
            copies.push((copy.pointer.clone(), item));
        }
        if copies.is_empty() {
            continue;
        }
        keeper["memory_review_needed"] = json!(independent_observations(&keeper) > before);
        keeper["memory_reconciled_at"] = json!(now.to_rfc3339());
        if let Some(slot) = document.pointer_mut(&fold.keeper.pointer) {
            *slot = keeper;
        }
        for (pointer, item) in copies {
            if let Some(slot) = document.pointer_mut(&pointer) {
                *slot = item;
                count += 1;
            }
        }
    }
    count
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub pointer: String,
    pub id: String,
    pub revision: String,
    pub item: Value,
}

/// Enumerate canonical structured collections, excluding private journal data.
/// This also supports agent tier documents through their `fields` wrapper.
pub fn sources(document: &Value) -> Vec<Source> {
    let mut result = Vec::new();
    let mut todo = vec![(String::new(), document)];
    while let Some((pointer, value)) = todo.pop() {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if key.starts_with('_') {
                        continue;
                    }
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    todo.push((format!("{pointer}/{escaped}"), value));
                }
            },
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    if item.is_object() && !retired(item) {
                        result.push(Source {
                            pointer: format!("{pointer}/{index}"),
                            id: if item.get("memory_record_id").is_some() {
                                record_id(item)
                            } else {
                                digest(&json!([pointer, record_id(item)]))
                            },
                            revision: digest(item),
                            item: item.clone(),
                        });
                    }
                }
            },
            _ => {},
        }
    }
    result.sort_by(|a, b| a.pointer.cmp(&b.pointer));
    result
}

/// Lazily bring pre-lifecycle primitive collections into the same review path.
/// Preserve their current status during migration; only new writes are pending.
pub fn normalize_legacy_collections(document: &mut Value, now: DateTime<Utc>) -> usize {
    fn walk(value: &mut Value, path: &str, now: DateTime<Utc>) -> usize {
        match value {
            Value::Object(map) => map
                .iter_mut()
                .filter(|(key, _)| !key.starts_with('_'))
                .map(|(key, value)| walk(value, &format!("{path}/{key}"), now))
                .sum(),
            Value::Array(items) if items.iter().any(|item| !item.is_object()) => {
                let count = items.iter().filter(|item| !item.is_object()).count();
                *items = merge_collection(items, &[], path, now);
                count
            },
            _ => 0,
        }
    }
    walk(document, "", now)
}

/// Whether [`normalize_legacy_collections`] would change `document`, without
/// copying or mutating it (same walk: non-journal object keys, arrays holding
/// a primitive item).
pub fn has_legacy_collections(document: &Value) -> bool {
    match document {
        Value::Object(map) => map
            .iter()
            .filter(|(key, _)| !key.starts_with('_'))
            .any(|(_, value)| has_legacy_collections(value)),
        Value::Array(items) => items.iter().any(|item| !item.is_object()),
        _ => false,
    }
}

/// The recognizable control payload [`quarantine_legacy_schema_echoes`] moves.
fn is_legacy_schema_echo(map: &serde_json::Map<String, Value>) -> bool {
    map.get("allowed_user_tiers").is_some_and(Value::is_array)
        && map.get("promotion_rule").is_some_and(Value::is_string)
        && map.values().any(|v| {
            v.get("type") == Some(&json!("collection"))
                && v.get("item_schema").is_some_and(Value::is_object)
        })
        && map.iter().all(|(key, v)| {
            matches!(key.as_str(), "allowed_user_tiers" | "promotion_rule")
                || v.get("type") == Some(&json!("collection"))
                    && v.get("item_schema").is_some_and(Value::is_object)
        })
}

/// Whether [`quarantine_legacy_schema_echoes`] would change `document`,
/// without copying or mutating it.
pub fn has_legacy_schema_echoes(document: &Value) -> bool {
    let Some(map) = document.as_object() else {
        return false;
    };
    is_legacy_schema_echo(map)
        || map
            .iter()
            .filter(|(key, _)| !key.starts_with('_'))
            .any(|(_, child)| has_legacy_schema_echoes(child))
}

/// Old promotion prompts could echo their collection descriptor into storage.
/// Preserve that exact, recognizable control payload as audit data instead of
/// teaching its allowed tier names to the model as owner preferences.
pub fn quarantine_legacy_schema_echoes(document: &mut Value, now: DateTime<Utc>) -> usize {
    fn walk(value: &mut Value, path: &str, found: &mut Vec<(String, Value)>) {
        if let Some(map) = value.as_object() {
            let schema_echo = is_legacy_schema_echo(map);
            if schema_echo {
                found.push((
                    path.into(),
                    std::mem::replace(
                        value,
                        if path.is_empty() {
                            json!({})
                        } else {
                            json!([])
                        },
                    ),
                ));
                return;
            }
        }
        if let Some(map) = value.as_object_mut() {
            for (key, child) in map.iter_mut().filter(|(key, _)| !key.starts_with('_')) {
                walk(child, &format!("{path}/{key}"), found);
            }
        }
    }
    let mut found = Vec::new();
    walk(document, "", &mut found);
    let count = found.len();
    for (path, value) in found {
        let id = digest(&json!([path, value]));
        document[JOURNAL]["quarantined_schemas"][&id] =
            json!({"path":path,"value":value,"at":now.to_rfc3339()});
    }
    count
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Fact,
    Preference,
    Goal,
    Instruction,
    Observation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Duplicate,
    Reinforce,
    Coexist,
    Supersede,
    #[serde(rename = "ask_owner", alias = "clarify")]
    Clarify,
}

/// Coverage is directional and independent of the relationship label. Supporting
/// one existing assertion does not make a compound incoming statement redundant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncomingCoverage {
    Full,
    Partial,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    pub existing_id: String,
    pub relation: Relation,
    #[serde(default)]
    pub incoming_coverage: IncomingCoverage,
    pub same_subject_and_aspect: bool,
    pub same_context: bool,
    pub explicit_correction: bool,
    pub confidence: f64,
    pub rationale: String,
    pub question: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub kind: Kind,
    pub subject: String,
    pub aspect: String,
    #[serde(default, deserialize_with = "optional_context")]
    pub context: String,
    pub valid_until: Option<String>,
    pub validity_quote: Option<String>,
    pub relationships: Vec<Relationship>,
}

fn optional_context<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub incoming: Source,
    pub existing: Source,
    pub question: String,
    pub reason: String,
    pub state: String,
    pub created_at: i64,
    pub context_revision: String,
}

pub fn context_revision(document: &Value, anchors: &[&Source]) -> String {
    digest(&json!(sources(document)
        .iter()
        .filter(|s| anchors.iter().any(|anchor| {
            s.id == anchor.id
                || (["project_id", "engagement_scope", "scope", "subject"]
                    .iter()
                    .all(|key| s.item.get(key) == anchor.item.get(key))
                    && (s.item.get("key").is_some() && s.item.get("key") == anchor.item.get("key")
                        || s.item.get("memory_aspect").is_some()
                            && s.item.get("memory_aspect") == anchor.item.get("memory_aspect")
                            && s.item.get("memory_subject") == anchor.item.get("memory_subject")))
        }))
        .map(|s| (&s.pointer, &s.revision))
        .collect::<Vec<_>>()))
}

#[derive(Debug, Default, Serialize)]
pub struct ApplyOutcome {
    pub applied: bool,
    pub stale: bool,
    pub conflicts: Vec<Conflict>,
    /// Other copies of the merged claim retired into the kept record.
    pub folded: usize,
}

fn current(document: &Value, source: &Source) -> bool {
    document
        .pointer(&source.pointer)
        .is_some_and(|item| digest(item) == source.revision)
}

/// Retire `item` because the owner restated the same keyed preference with a
/// different claim in their own turn. The caller checks the owner's word
/// (`owner_turn::owner_turn_may_retire`); this only records the supersession.
pub fn supersede_by_owner_restatement(item: &mut Value, successor: &str, now: DateTime<Utc>) {
    supersede(item, successor, "owner_restated_preference", now);
    item["supersession_source"] = json!("owner_restatement");
}

fn supersede(item: &mut Value, successor: &str, reason: &str, now: DateTime<Utc>) {
    item["memory_lifecycle"] = json!("superseded");
    item["superseded_id"] = json!(record_id(item));
    item["superseded_by"] = json!(successor);
    item["superseded_at"] = json!(now.to_rfc3339());
    item["supersession_reason"] = json!(reason);
    item["supersession_source"] = json!(OPERATION);
    item["memory_review_needed"] = json!(false);
}

fn incoming_is_covered(incoming: &Source, existing: &Source, relationship: &Relationship) -> bool {
    relationship.incoming_coverage == IncomingCoverage::Full
        || (text(&incoming.item) == text(&existing.item)
            && applicability(&incoming.item) == applicability(&existing.item))
}

fn stored_scope_matches(incoming: &Source, existing: &Source) -> bool {
    ["project_id", "engagement_scope", "scope", "subject"]
        .iter()
        .all(|key| existing.item.get(key) == incoming.item.get(key))
}

/// Whether `apply_review` would merge the incoming record into `existing`
/// (outside an owner clarification): the same checks as its merge arm.
fn merge_ready(
    incoming: &Source,
    revised: &Value,
    existing: &Source,
    relationship: &Relationship,
) -> bool {
    matches!(
        relationship.relation,
        Relation::Duplicate | Relation::Reinforce
    ) && relationship.same_subject_and_aspect
        && relationship.same_context
        && stored_scope_matches(incoming, existing)
        && relationship.confidence >= 0.9
        && explicit(&incoming.item) == explicit(&existing.item)
        && incoming_is_covered(incoming, existing, relationship)
        && ["valid_from", "valid_until"].iter().all(|key| {
            existing.item.get(key).filter(|v| !v.is_null())
                == revised.get(key).filter(|v| !v.is_null())
        })
}

fn sustained_observation_evidence(item: &Value) -> bool {
    independent_observations(item) >= 3 && observation_span_days(item) >= 2
}

fn concrete_question(relationship: &Relationship) -> Option<&str> {
    relationship
        .question
        .as_deref()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 600)
}

/// Apply the whole relationship plan after validating every source revision.
/// Partial application would turn a multi-claim correction into inconsistent truth.
pub fn apply_review(
    document: &mut Value,
    incoming: &Source,
    offered: &[Source],
    review: &Review,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, String> {
    let mut selected = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if review.relationships.len() > 16
        || review.subject.trim().is_empty()
        || review.aspect.trim().is_empty()
    {
        return Err("missing or excessive memory relationship data".into());
    }
    for relationship in &review.relationships {
        let existing = offered
            .iter()
            .find(|s| s.id == relationship.existing_id && s.id != incoming.id)
            .ok_or("review referenced an unoffered memory")?;
        if !seen.insert(existing.id.clone())
            || !relationship.confidence.is_finite()
            || !(0.0..=1.0).contains(&relationship.confidence)
            || relationship.rationale.trim().is_empty()
        {
            return Err("invalid or repeated relationship".into());
        }
        if relationship.relation == Relation::Clarify && concrete_question(relationship).is_none() {
            return Err(
                "asking the owner requires a concrete question of at most 600 characters".into(),
            );
        }
        selected.push((existing, relationship));
    }
    let merges = selected
        .iter()
        .filter(|(s, r)| {
            matches!(r.relation, Relation::Duplicate | Relation::Reinforce)
                && incoming_is_covered(incoming, s, r)
        })
        .count();
    if merges > 0
        && selected
            .iter()
            .any(|(_, r)| r.relation == Relation::Clarify)
    {
        return Err("a merge cannot also dispute another claim in the same plan".into());
    }
    if !current(document, incoming) || selected.iter().any(|(s, _)| !current(document, s)) {
        return Ok(ApplyOutcome {
            stale: true,
            ..Default::default()
        });
    }
    let resolving = incoming
        .item
        .get("resolves_conflict")
        .and_then(Value::as_str)
        .and_then(|id| {
            serde_json::from_value::<Conflict>(document[JOURNAL]["conflicts"][id].clone()).ok()
        });
    if let Some(conflict) = &resolving {
        if conflict.state != "clarifying"
            || [&conflict.incoming.id, &conflict.existing.id]
                .iter()
                .any(|id| !selected.iter().any(|(s, _)| &s.id == *id))
        {
            return Err("clarification must reconcile both cited memories".into());
        }
    }
    let mut revised = incoming.item.clone();
    revised["memory_record_id"] = json!(incoming.id);
    revised["memory_evidence"] = json!(evidence(&incoming.item));
    revised["memory_kind"] = json!(review.kind);
    revised["memory_subject"] = json!(review.subject);
    revised["memory_aspect"] = json!(review.aspect);
    revised["memory_context"] = json!(review.context);
    revised["memory_review_needed"] = json!(false);
    revised["memory_review_version"] = json!(POLICY_VERSION);
    revised["memory_reconciled_at"] = json!(now.to_rfc3339());
    revised["memory_lifecycle"] = json!("active");
    if let Some(until) = &review.valid_until {
        let quote = review
            .validity_quote
            .as_deref()
            .filter(|q| !q.trim().is_empty() && text(&incoming.item).contains(q))
            .ok_or("expiry requires quoted source evidence")?;
        let _ = quote;
        DateTime::parse_from_rfc3339(until).map_err(|_| "invalid validity date")?;
        revised["valid_until"] = json!(until);
    }
    let mut outcome = ApplyOutcome::default();
    let mut changes = Vec::new();
    // The incoming record merges into one memory. When it repeats several
    // (they are copies of each other), it merges into the one the reviewer is
    // surest of, earliest offered (closest in meaning) on a tie; the other
    // copies the reviewer called duplicates fold into that one too, and the
    // rest stay as they are. These plans used to be rejected outright, so a
    // record with two saved copies could never be reviewed.
    let (kept, extra_copies) = if resolving.is_some() {
        if merges > 1 {
            return Err("an owner clarification can merge into one memory only".into());
        }
        (None, Vec::new())
    } else {
        let mut ready: Vec<(usize, &Source, &Relationship)> = selected
            .iter()
            .filter(|(s, r)| merge_ready(incoming, &revised, s, r))
            .map(|(s, r)| {
                let at = offered
                    .iter()
                    .position(|o| o.id == s.id)
                    .unwrap_or(usize::MAX);
                (at, *s, *r)
            })
            .collect();
        ready.sort_by(|a, b| {
            b.2.confidence
                .total_cmp(&a.2.confidence)
                .then(a.0.cmp(&b.0))
        });
        if ready.len() > 1 {
            let (_, kept, _) = ready.remove(0);
            (
                Some(kept),
                ready.into_iter().map(|(_, s, r)| (s, r)).collect(),
            )
        } else {
            (None, Vec::new())
        }
    };
    for (source, relationship) in selected {
        if extra_copies.iter().any(|(s, _)| s.id == source.id) {
            continue;
        }
        let mut existing = source.item.clone();
        existing["memory_record_id"] = json!(source.id);
        let stored_scope_matches = stored_scope_matches(incoming, source);
        let eligible = relationship.same_subject_and_aspect
            && relationship.same_context
            && stored_scope_matches
            && relationship.confidence >= 0.9;
        match relationship.relation {
            Relation::Duplicate | Relation::Reinforce if eligible => {
                // A model cannot launder observations into an explicit statement.
                if explicit(&incoming.item) != explicit(&existing) {
                    continue;
                }
                if resolving
                    .as_ref()
                    .is_some_and(|c| source.id == c.incoming.id || source.id == c.existing.id)
                    && explicit(&incoming.item)
                {
                    // A reaffirmation can also explain an exception. Keep the
                    // owner's complete answer as the current claim rather than
                    // hiding its qualifications in the older record's evidence.
                    merge_evidence(&mut revised, &existing);
                    for key in ["valid_from", "valid_until"] {
                        if revised.get(key).is_none_or(Value::is_null) {
                            if let Some(value) = existing.get(key) {
                                revised[key] = value.clone();
                            }
                        }
                    }
                    supersede(&mut existing, &incoming.id, "owner_clarification", now);
                    changes.push((source.pointer.clone(), existing));
                    continue;
                }
                // The relation describes overlap, not permission to discard the
                // entire incoming record. Missing/partial coverage keeps both
                // records current without copying unrelated evidence into the
                // old claim. Explicit validity boundaries also remain distinct.
                if !incoming_is_covered(incoming, source, relationship)
                    || ["valid_from", "valid_until"].iter().any(|key| {
                        existing.get(key).filter(|v| !v.is_null())
                            != revised.get(key).filter(|v| !v.is_null())
                    })
                {
                    continue;
                }
                merge_evidence(&mut existing, &revised);
                if resolving
                    .as_ref()
                    .is_some_and(|c| source.id == c.incoming.id || source.id == c.existing.id)
                {
                    existing["memory_lifecycle"] = json!("active");
                }
                existing["memory_review_needed"] = json!(
                    independent_observations(&existing) > independent_observations(&source.item)
                );
                supersede(
                    &mut revised,
                    &source.id,
                    "duplicate_or_supporting_evidence",
                    now,
                );
                changes.push((source.pointer.clone(), existing));
            },
            Relation::Supersede
                if eligible
                    && ((explicit(&incoming.item) && relationship.explicit_correction)
                        || (!explicit(&existing)
                            && !explicit(&incoming.item)
                            && !matches!(
                                existing.get("memory_kind").and_then(Value::as_str),
                                Some("goal" | "instruction")
                            )
                            && incoming
                                .item
                                .get("source_type")
                                .and_then(Value::as_str)
                                .is_none_or(|source| {
                                    super::memory_provenance::MemoryTrust::from_source_type(source)
                                        != super::memory_provenance::MemoryTrust::Untrusted
                                })
                            && sustained_observation_evidence(&incoming.item))) =>
            {
                supersede(&mut existing, &incoming.id, &relationship.rationale, now);
                changes.push((source.pointer.clone(), existing));
            },
            Relation::Supersede
                if relationship.same_subject_and_aspect
                    && stored_scope_matches
                    && !explicit(&existing)
                    && !explicit(&incoming.item) =>
            {
                // An emerging contrary pattern is retained but cannot displace
                // the old inference until independent evidence accumulates.
                revised["memory_lifecycle"] = json!("pending_review");
                revised["memory_review_needed"] = json!(false);
            },
            Relation::Clarify
                if relationship.same_subject_and_aspect
                    && stored_scope_matches
                    && !explicit(&existing)
                    && !explicit(&incoming.item)
                    && !sustained_observation_evidence(&incoming.item) =>
            {
                // Choosing "clarify" cannot bypass the evidence floor for a
                // change between inferred claims or unsettle the older claim.
                // Independent new evidence can resume this pending review.
                revised["memory_lifecycle"] = json!("pending_review");
                revised["memory_review_needed"] = json!(false);
            },
            Relation::Clarify | Relation::Supersede => {
                if !relationship.same_subject_and_aspect || !stored_scope_matches {
                    continue;
                }
                if relationship.confidence < 0.85 {
                    revised["memory_lifecycle"] = json!("pending_review");
                    revised["memory_review_needed"] = json!(true);
                    continue;
                }
                // Only a blocked replacement can use the confirmation fallback.
                // An ask_owner proposal must supply its own concrete question.
                let question = concrete_question(relationship).map(str::to_owned)
                    .unwrap_or_else(||format!("Does your earlier memory still apply, or has it changed? Earlier: {}. New information: {}.",
                        text(&existing).chars().take(200).collect::<String>(),text(&revised).chars().take(200).collect::<String>()));
                revised["memory_lifecycle"] = json!("unresolved");
                existing["memory_lifecycle"] = json!("unresolved");
                let id = digest(&json!([
                    POLICY_VERSION,
                    incoming.id,
                    source.id,
                    evidence(&incoming.item),
                    evidence(&source.item)
                ]));
                revised["memory_conflict_id"] = json!(id);
                existing["memory_conflict_id"] = json!(id);
                changes.push((source.pointer.clone(), existing));
                outcome.conflicts.push(Conflict {
                    id,
                    incoming: incoming.clone(),
                    existing: source.clone(),
                    question,
                    reason: relationship.rationale.clone(),
                    state: "pending".into(),
                    created_at: now.timestamp(),
                    context_revision: String::new(),
                });
            },
            Relation::Coexist
                if resolving
                    .as_ref()
                    .is_some_and(|c| source.id == c.incoming.id || source.id == c.existing.id) =>
            {
                existing["memory_lifecycle"] = json!("active");
                changes.push((source.pointer.clone(), existing));
            },
            _ => {},
        }
    }
    if let Some(kept) = kept {
        let mut folded_copies = Vec::new();
        if let Some((_, kept_value)) = changes.iter_mut().find(|(p, _)| p == &kept.pointer) {
            for (copy, relationship) in &extra_copies {
                // Only a copy the reviewer called the same claim folds away; a
                // supporting one may say more, so it stays in service.
                if relationship.relation != Relation::Duplicate {
                    continue;
                }
                merge_evidence(kept_value, &copy.item);
                let mut folded = copy.item.clone();
                folded["memory_record_id"] = json!(copy.id);
                supersede(&mut folded, &kept.id, "duplicate_of_merged_copy", now);
                outcome.folded += 1;
                folded_copies.push((copy.pointer.clone(), folded));
            }
            kept_value["memory_review_needed"] =
                json!(independent_observations(kept_value) > independent_observations(&kept.item));
        }
        changes.extend(folded_copies);
    }
    for (pointer, value) in changes {
        let mut value = value;
        value["memory_reconciled_at"] = json!(now.to_rfc3339());
        *document.pointer_mut(&pointer).ok_or("source disappeared")? = value;
    }
    *document
        .pointer_mut(&incoming.pointer)
        .ok_or("incoming disappeared")? = revised;
    if let Some(conflict) = resolving {
        if outcome.conflicts.is_empty()
            && state(document.pointer(&incoming.pointer).unwrap()) != "pending_review"
        {
            document[JOURNAL]["conflicts"][&conflict.id]["state"] = json!("resolved");
        }
    }
    // Capture versions after *all* changes, so an answer cannot approve a newer
    // mutation under the evidence shown by an older question.
    for conflict in &mut outcome.conflicts {
        for source in [&mut conflict.incoming, &mut conflict.existing] {
            source.item = document.pointer(&source.pointer).unwrap().clone();
            source.revision = digest(&source.item);
        }
        conflict.context_revision =
            context_revision(document, &[&conflict.incoming, &conflict.existing]);
        document[JOURNAL]["conflicts"][&conflict.id] = json!(conflict);
    }
    expire(document, now);
    outcome.applied = true;
    Ok(outcome)
}

/// Whether a live record's explicit validity interval has ended; what
/// [`expire`] retires.
pub fn validity_ended(item: &Value, now: DateTime<Utc>) -> bool {
    item.get("valid_until")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|until| until <= now)
}

/// Time passage retires only explicit validity intervals, never a standing goal
/// just because it was not recalled or followed recently.
pub fn expire(document: &mut Value, now: DateTime<Utc>) -> usize {
    let mut count = 0;
    for source in sources(document) {
        if validity_ended(&source.item, now) {
            let item = document.pointer_mut(&source.pointer).unwrap();
            item["memory_lifecycle"] = json!("expired");
            item["retired_at"] = json!(now.to_rfc3339());
            count += 1;
        }
    }
    count
}

/// Owner choices are deterministic and scoped by the document being locked.
/// Free text is staged as evidence for review, never interpreted as blanket consent.
pub fn answer_conflict(
    document: &mut Value,
    id: &str,
    decision: &str,
    input: Option<&str>,
    now: DateTime<Utc>,
) -> Result<bool, String> {
    let value = document[JOURNAL]["conflicts"][id].clone();
    let mut conflict: Conflict = serde_json::from_value(value).map_err(|_| "unknown conflict")?;
    if conflict.state != "pending" {
        return Ok(false);
    }
    if !current(document, &conflict.incoming)
        || !current(document, &conflict.existing)
        || context_revision(document, &[&conflict.incoming, &conflict.existing])
            != conflict.context_revision
        || now.timestamp() - conflict.created_at > 7 * 86400
    {
        conflict.state = "stale".into();
        document[JOURNAL]["conflicts"][id] = json!(conflict);
        return Ok(false);
    }
    let (winner, loser) = match decision {
        "keep_existing" => (Some(&conflict.existing), Some(&conflict.incoming)),
        "use_new" => (Some(&conflict.incoming), Some(&conflict.existing)),
        "answer" => {
            let words = input
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.chars().count() <= 2000)
                .ok_or("clarification requires bounded owner input")?;
            let old = document.pointer(&conflict.incoming.pointer).unwrap();
            let mut clarification = json!({"key":format!("memory_clarification:{id}"),"value":words,
                "source_type":"explicit_user_statement","source_event_id":format!("memory_answer:{id}"),
                "updated_at":now.to_rfc3339(),"resolves_conflict":id,
                "clarification_context":[text(&conflict.existing.item),text(old)]});
            for field in ["project_id", "engagement_scope", "scope", "subject"] {
                if let Some(value) = old.get(field) {
                    clarification[field] = value.clone();
                }
            }
            let parent = conflict
                .incoming
                .pointer
                .rsplit_once('/')
                .ok_or("invalid source path")?
                .0;
            let array = document
                .pointer_mut(parent)
                .and_then(Value::as_array_mut)
                .ok_or("missing collection")?;
            *array = merge_items(array, &[clarification], now);
            (None, None)
        },
        "dismiss" => (None, None),
        _ => return Err("unknown memory answer".into()),
    };
    if let (Some(winner), Some(loser)) = (winner, loser) {
        let item = document.pointer_mut(&winner.pointer).unwrap();
        item["memory_lifecycle"] = json!("active");
        item["source_type"] = json!("owner_confirmed");
        item["owner_confirmed_at"] = json!(now.to_rfc3339());
        item["memory_reconciled_at"] = json!(now.to_rfc3339());
        supersede(
            document.pointer_mut(&loser.pointer).unwrap(),
            &winner.id,
            "owner_resolved_conflict",
            now,
        );
    }
    conflict.state = if decision == "dismiss" {
        "dismissed"
    } else if decision == "answer" {
        "clarifying"
    } else {
        "resolved"
    }
    .into();
    document[JOURNAL]["conflicts"][id] = json!(conflict);
    Ok(true)
}
