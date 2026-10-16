//! Native user knowledge (`memory/users/knowledge.json`) as intentions.
//!
//! # What is an intention here
//!
//! Kind comes only from structured fields, never from wording:
//!
//! - a reviewed entry's `memory_kind` (set by the lifecycle reviewer):
//!   `preference` → taste, `goal` → goal, `instruction` → standing
//!   instruction; `fact` and `observation` are not intentions;
//! - an unreviewed entry takes its tier's default (`preferences` → taste,
//!   `goals` → goal, `channel_writing_preferences` → standing instruction);
//!   entries in other tiers are intentions only once reviewed as one.
//!
//! A standing instruction needs direct or confirmed owner evidence (P0
//! contracts §4); one that is only inferred is presented as a taste.
//!
//! # Authority
//!
//! `source_type` is written by whoever produced the entry, and some producers
//! are model tools. The adapter therefore lowers what it cannot verify:
//!
//! | Stored | Presented as |
//! |---|---|
//! | untrusted source, `trust: untrusted`, or an untrusted origin surface | `inferred_untrusted` |
//! | `owner_confirmed` with a host confirmation stamp | `confirmed` |
//! | `explicit_user_statement` / `correction` with owner-turn evidence | `direct_owner_statement` |
//! | anything else, including the above without their evidence | `inferred` |
//!
//! Owner-turn evidence is a `memory_evidence` entry with `source_role: user`,
//! a `source_id` and a non-empty `quote`. Tool ingress strips model-supplied
//! `memory_*` fields (`memory_lifecycle::provenance::reset_proposal_metadata`),
//! so such an entry is host-recorded: today the consolidator's grounded chat
//! quotes produce it. Nothing in storage is rewritten.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use crate::magician_v2::agents::memory_lifecycle::{self, Kind};
use crate::magician_v2::agents::memory_provenance::MemoryTrust;
use crate::magician_v2::agents::memory_scope::MemoryScope;
use crate::magician_v2::agents::AgentMemoryService;
use crate::magician_v2::apps::memory_bridge::APP_SOURCE_ELIGIBILITY_METADATA_KEY;
use crate::magician_v2::evidence::tier_contracts::CHANNEL_WRITING_PREFERENCES_TIER_NAME;

use super::super::intention::{
    AssertionStatus, Coverage, CoverageStatus, EvidenceRef, EvidenceRole, Intention, IntentionKind,
    IntentionLifecycle, IntentionSource, INTENTION_SCHEMA_VERSION,
};
use super::SourceCollection;

/// Coverage name for this source.
pub const NATIVE_SOURCE: &str = "native_knowledge";

/// Tiers whose unreviewed entries are intentions. `goals` is not in
/// `USER_MEMORY_TIERS` today; it is listed so a goals tier is covered the day
/// one is written, instead of being silently skipped.
const TIER_DEFAULT_KINDS: &[(&str, IntentionKind)] = &[
    ("preferences", IntentionKind::Taste),
    ("goals", IntentionKind::Goal),
    (
        CHANNEL_WRITING_PREFERENCES_TIER_NAME,
        IntentionKind::StandingInstruction,
    ),
];

fn tier_default_kind(tier: &str) -> Option<IntentionKind> {
    TIER_DEFAULT_KINDS
        .iter()
        .find(|(name, _)| *name == tier)
        .map(|(_, kind)| *kind)
}

/// Read the owner's knowledge through the memory service and project it.
/// A load failure is `unavailable`, never an empty inventory.
pub async fn load_native_intentions(
    memory: &AgentMemoryService,
    now: DateTime<Utc>,
) -> SourceCollection {
    match memory.load_user_knowledge().await {
        Ok(knowledge) => project_native_intentions(&knowledge, now),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "preference alignment: user knowledge could not be read"
            );
            SourceCollection::unavailable(NATIVE_SOURCE)
        },
    }
}

/// Project exact `knowledge.json` bytes, as returned with a generation by
/// `generation::read_intention_generation_with_knowledge`. `None` is a store
/// with no knowledge file yet: complete and empty.
pub fn project_native_knowledge_bytes(
    bytes: Option<&[u8]>,
    now: DateTime<Utc>,
) -> SourceCollection {
    let Some(bytes) = bytes else {
        return SourceCollection::empty_complete();
    };
    match serde_json::from_slice::<Value>(bytes) {
        Ok(knowledge) => project_native_intentions(&knowledge, now),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "preference alignment: user knowledge is not valid JSON"
            );
            SourceCollection::unavailable(NATIVE_SOURCE)
        },
    }
}

/// Project every structurally eligible entry. Entries that might be
/// intentions but cannot be read safely are listed in `omitted_ids` and make
/// coverage partial; they are never dropped silently.
pub fn project_native_intentions(knowledge: &Value, now: DateTime<Utc>) -> SourceCollection {
    let Some(tiers) = knowledge.as_object() else {
        return SourceCollection::unavailable(NATIVE_SOURCE);
    };
    let mut intentions = Vec::new();
    let mut omitted_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for (tier, value) in tiers {
        // `_memory_lifecycle` and other private journals are not knowledge.
        if tier.starts_with('_') {
            continue;
        }
        let default_kind = tier_default_kind(tier);
        let Some(items) = value.as_array() else {
            // An intention-bearing tier in a shape this adapter cannot read
            // (a legacy key→value map, say) may hold intentions.
            if default_kind.is_some() && !is_blank(value) {
                omitted_ids.push(format!("tier:{tier}"));
            }
            continue;
        };
        for item in items {
            match project_item(tier, item, now) {
                Projection::Intention(intention) => {
                    if seen.insert(intention.id.clone()) {
                        intentions.push(*intention);
                    } else {
                        omitted_ids.push(intention.id);
                    }
                },
                Projection::Omitted(id) => omitted_ids.push(id),
                Projection::NotEligible => {},
            }
        }
    }
    SourceCollection {
        intentions,
        coverage: Coverage {
            status: if omitted_ids.is_empty() {
                CoverageStatus::Complete
            } else {
                CoverageStatus::Partial
            },
            omitted_ids,
            unavailable_sources: Vec::new(),
        },
    }
}

enum Projection {
    Intention(Box<Intention>),
    /// Possibly an intention, but not safely readable.
    Omitted(String),
    /// Structurally not an intention right now (a fact, retired, expired).
    NotEligible,
}

/// The kind an entry in `tier` is read as, before any evidence check, or
/// `None` when it is not an intention at all.
pub fn intention_kind(tier: &str, item: &Value) -> Option<IntentionKind> {
    if tier.starts_with('_') {
        return None;
    }
    match item
        .get("memory_kind")
        .cloned()
        .map(serde_json::from_value::<Kind>)
    {
        Some(Ok(Kind::Preference)) => Some(IntentionKind::Taste),
        Some(Ok(Kind::Goal)) => Some(IntentionKind::Goal),
        Some(Ok(Kind::Instruction)) => Some(IntentionKind::StandingInstruction),
        Some(Ok(Kind::Fact | Kind::Observation)) => None,
        // Unreviewed (or an unrecognized label): the tier decides.
        Some(Err(_)) | None => tier_default_kind(tier),
    }
}

/// Whether an entry is read as an intention in any lifecycle: the set the
/// preference history and the owner view follow. Eligibility for a check
/// (lifecycle, validity, app governance) applies on top.
pub fn bears_intention(tier: &str, item: &Value) -> bool {
    intention_kind(tier, item).is_some()
}

/// Whether an intention-bearing entry is offered to checks at `now`, and if
/// not, why. The projection and the owner view share it.
#[derive(Debug, Clone, PartialEq)]
pub enum Eligibility {
    Eligible {
        lifecycle: IntentionLifecycle,
        valid_until: Option<DateTime<Utc>>,
    },
    /// Superseded, replaced, expired by lifecycle, or retracted.
    Retired,
    /// Outside its validity window.
    OutOfWindow,
    /// Possibly in force, but not safely readable: the reason code.
    Omitted(&'static str),
}

pub fn eligibility(item: &Value, now: DateTime<Utc>) -> Eligibility {
    let lifecycle = if !item.is_object() {
        // A pre-lifecycle primitive entry is current until migrated.
        IntentionLifecycle::Active
    } else if memory_lifecycle::retired(item) {
        return Eligibility::Retired;
    } else {
        match memory_lifecycle::state(item) {
            "active" => IntentionLifecycle::Active,
            "pending_review" => IntentionLifecycle::PendingReview,
            "unresolved" => IntentionLifecycle::Unresolved,
            _ => return Eligibility::Omitted("unknown_lifecycle"),
        }
    };
    // App-governed entries must have their source eligibility re-resolved
    // before use; app memory is not an alignment source in v1.
    if item.get(APP_SOURCE_ELIGIBILITY_METADATA_KEY).is_some() {
        return Eligibility::Omitted("app_governed");
    }
    match validity_window(item, now) {
        Window::Open(valid_until) => Eligibility::Eligible {
            lifecycle,
            valid_until,
        },
        Window::Closed => Eligibility::OutOfWindow,
        Window::Unreadable => Eligibility::Omitted("unreadable_validity"),
    }
}

fn project_item(tier: &str, item: &Value, now: DateTime<Utc>) -> Projection {
    let id = intention_id(tier, item);

    let Some(kind) = intention_kind(tier, item) else {
        return Projection::NotEligible;
    };

    let (lifecycle, valid_until) = match eligibility(item, now) {
        Eligibility::Eligible {
            lifecycle,
            valid_until,
        } => (lifecycle, valid_until),
        Eligibility::Retired | Eligibility::OutOfWindow => return Projection::NotEligible,
        Eligibility::Omitted(_) => return Projection::Omitted(id),
    };

    let assertion_status = assertion_status(item);
    let kind = if kind == IntentionKind::StandingInstruction && !assertion_status.may_hold() {
        IntentionKind::Taste
    } else {
        kind
    };

    Projection::Intention(Box::new(Intention {
        schema_version: INTENTION_SCHEMA_VERSION.to_string(),
        id,
        revision: memory_lifecycle::digest(item),
        source: IntentionSource::NativeTier {
            tier: tier.to_string(),
        },
        kind,
        assertion_status,
        statement: render_statement(item),
        conditions: str_field(item, "memory_context").map(str::to_owned),
        scope: entry_scope(item),
        lifecycle,
        valid_until,
        evidence_refs: evidence_refs(item, assertion_status == AssertionStatus::InferredUntrusted),
        intervention_policy: None,
    }))
}

/// The lifecycle record id where one exists. A pre-lifecycle entry gets an
/// id derived the way `memory_lifecycle` derives identity (key, normalized
/// claim, applicability, source), qualified by tier, so it is stable across
/// reads and array reordering.
pub fn intention_id(tier: &str, item: &Value) -> String {
    match str_field(item, "memory_record_id") {
        Some(id) => id.to_string(),
        None => format!(
            "legacy:{}",
            memory_lifecycle::digest(&json!([tier, memory_lifecycle::record_id(item)]))
        ),
    }
}

/// The force an entry's evidence can carry. See the module table.
pub fn assertion_status(item: &Value) -> AssertionStatus {
    let source_type = item
        .get("source_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if is_untrusted(item, MemoryTrust::from_source_type(source_type)) {
        return AssertionStatus::InferredUntrusted;
    }
    match source_type.trim().to_ascii_lowercase().as_str() {
        "owner_confirmed" if has_confirmation_stamp(item) => AssertionStatus::Confirmed,
        "explicit_user_statement" | "correction" if has_owner_turn_evidence(item) => {
            AssertionStatus::DirectOwnerStatement
        },
        _ => AssertionStatus::Inferred,
    }
}

fn is_untrusted(item: &Value, declared: MemoryTrust) -> bool {
    declared == MemoryTrust::Untrusted
        || str_field(item, "trust").is_some_and(|trust| trust.eq_ignore_ascii_case("untrusted"))
        || MemoryTrust::ceiling_for_origin_surface(str_field(item, "origin_surface"))
            == Some(MemoryTrust::Untrusted)
}

/// The owner confirmation paths stamp when they confirmed: the confirm
/// endpoint writes `confirmed_at`, a conflict answer `owner_confirmed_at`.
fn has_confirmation_stamp(item: &Value) -> bool {
    ["confirmed_at", "owner_confirmed_at"]
        .iter()
        .any(|key| item.get(*key).is_some_and(|value| !value.is_null()))
}

/// Host-recorded evidence that an owner turn said this: a `memory_evidence`
/// entry with the user role, the turn's source id and a quote from it that
/// grounds the record's value. A real owner line attached to a claim the
/// owner never made — a save a page talked the model into during the owner's
/// turn, or a distilled "fact" citing an unrelated line — is not the owner's
/// statement, and the record reads as inferred.
pub fn has_owner_turn_evidence(item: &Value) -> bool {
    let claim = item
        .get("value")
        .map(crate::magician_v2::preference_alignment::owner_turn::claim_text)
        .unwrap_or_default();
    item.get("memory_evidence")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                str_field(entry, "source_role")
                    .is_some_and(|role| role.eq_ignore_ascii_case("user"))
                    && str_field(entry, "source_id").is_some()
                    && str_field(entry, "quote").is_some_and(|quote| {
                        crate::magician_v2::preference_alignment::owner_turn::quote_grounds_claim(
                            quote, &claim,
                        )
                    })
            })
        })
}

fn evidence_refs(item: &Value, untrusted: bool) -> Vec<EvidenceRef> {
    let mut refs: Vec<EvidenceRef> = Vec::new();
    if let Some(entries) = item.get("memory_evidence").and_then(Value::as_array) {
        for entry in entries {
            let Some(source_id) = str_field(entry, "source_id").or_else(|| str_field(entry, "id"))
            else {
                continue;
            };
            let role = match str_field(entry, "source_role").map(str::to_ascii_lowercase) {
                // A user turn in an untrusted capture is a participant, not
                // the owner.
                Some(role) if role == "user" && !untrusted => EvidenceRole::Owner,
                Some(role) if role == "user" => EvidenceRole::External,
                Some(role) if role == "assistant" => EvidenceRole::Assistant,
                Some(role) if role == "tool" => EvidenceRole::Tool,
                _ => EvidenceRole::Unknown,
            };
            let evidence = EvidenceRef {
                source_id: source_id.to_string(),
                source_revision: str_field(entry, "source_revision").map(str::to_owned),
                role,
            };
            if !refs.contains(&evidence) {
                refs.push(evidence);
            }
        }
        return refs;
    }
    if let Some(source_id) = ["root_source_id", "source_event_id", "source_id"]
        .iter()
        .find_map(|key| str_field(item, key))
    {
        refs.push(EvidenceRef {
            source_id: source_id.to_string(),
            source_revision: None,
            role: EvidenceRole::Unknown,
        });
    }
    refs
}

/// The entry's per-entry `MemoryScope`, when it parses and says something.
/// An empty or malformed scope is no scope, never "applies to everything".
fn entry_scope(item: &Value) -> Option<Value> {
    let scope: MemoryScope = serde_json::from_value(item.get("scope")?.clone()).ok()?;
    if scope.is_empty() {
        return None;
    }
    serde_json::to_value(scope).ok()
}

/// Key and current meaning, rendered faithfully: strings verbatim, structured
/// values as compact JSON so negation and conditions inside them survive.
/// Review metadata and provenance are left out; they are evidence, not the
/// statement.
pub fn render_statement(item: &Value) -> String {
    let Some(object) = item.as_object() else {
        return value_text(item);
    };
    let meaning = match object.get("value") {
        Some(value) => value_text(value),
        None => ["text", "insight", "summary", "pattern", "directive"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| value_text(&Value::Object(meaning_fields(object)))),
    };
    match str_field(item, "key") {
        Some(key) => format!("{key}: {meaning}"),
        None => meaning,
    }
}

fn meaning_fields(object: &Map<String, Value>) -> Map<String, Value> {
    object
        .iter()
        .filter(|(key, _)| {
            !key.starts_with("memory_")
                && !key.starts_with("supersed")
                && !key.starts_with("retire")
                && !matches!(
                    key.as_str(),
                    "key"
                        | "source_ids"
                        | "source_id"
                        | "source_event_id"
                        | "root_source_id"
                        | "source_type"
                        | "rationale"
                        | "confidence"
                        | "updated_at"
                        | "trust"
                )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

enum Window {
    Open(Option<DateTime<Utc>>),
    Closed,
    Unreadable,
}

/// The entry's validity window. `valid_until` at or before now has ended
/// (the same boundary `memory_lifecycle::expire` uses); `valid_from` after
/// now has not begun. A window that cannot be parsed cannot be checked.
fn validity_window(item: &Value, now: DateTime<Utc>) -> Window {
    let (Ok(valid_until), Ok(valid_from)) = (
        parse_instant(item.get("valid_until")),
        parse_instant(item.get("valid_from")),
    ) else {
        return Window::Unreadable;
    };
    if valid_until.is_some_and(|until| until <= now) || valid_from.is_some_and(|from| from > now) {
        return Window::Closed;
    }
    Window::Open(valid_until)
}

fn parse_instant(value: Option<&Value>) -> Result<Option<DateTime<Utc>>, ()> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) if raw.trim().is_empty() => Ok(None),
        Some(Value::String(raw)) => DateTime::parse_from_rfc3339(raw.trim())
            .map(|instant| Some(instant.with_timezone(&Utc)))
            .map_err(|_| ()),
        Some(_) => Err(()),
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-09T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn project(knowledge: Value) -> SourceCollection {
        project_native_intentions(&knowledge, now())
    }

    fn find<'a>(collection: &'a SourceCollection, id: &str) -> &'a Intention {
        collection
            .intentions
            .iter()
            .find(|intention| intention.id == id)
            .unwrap_or_else(|| panic!("intention `{id}` missing"))
    }

    fn owner_quote() -> Value {
        json!([{
            "id": "turn-1",
            "source_role": "user",
            "source_id": "chat-turn-1:user",
            "source_type": "explicit_user_statement",
            "quote": "Always check with me before ordering meat for me."
        }])
    }

    #[test]
    fn eligible_lifecycles_are_kept_and_retired_ones_are_not_intentions() {
        let collection = project(json!({"preferences": [
            {"memory_record_id": "active", "key": "a", "value": "x"},
            {"memory_record_id": "explicit-active", "key": "b", "value": "x", "memory_lifecycle": "active"},
            {"memory_record_id": "pending", "key": "c", "value": "x", "memory_lifecycle": "pending_review"},
            {"memory_record_id": "unresolved", "key": "d", "value": "x", "memory_lifecycle": "unresolved"},
            {"memory_record_id": "superseded", "key": "e", "value": "x", "memory_lifecycle": "superseded"},
            {"memory_record_id": "retracted", "key": "f", "value": "x", "memory_lifecycle": "retracted"},
            {"memory_record_id": "expired", "key": "g", "value": "x", "memory_lifecycle": "expired"},
            {"memory_record_id": "replaced", "key": "h", "value": "x", "memory_lifecycle": "replaced"}
        ]}));
        let ids: Vec<&str> = collection
            .intentions
            .iter()
            .map(|intention| intention.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["active", "explicit-active", "pending", "unresolved"]
        );
        assert_eq!(
            find(&collection, "active").lifecycle,
            IntentionLifecycle::Active
        );
        assert_eq!(
            find(&collection, "pending").lifecycle,
            IntentionLifecycle::PendingReview
        );
        assert_eq!(
            find(&collection, "unresolved").lifecycle,
            IntentionLifecycle::Unresolved
        );
        assert_eq!(collection.coverage.status, CoverageStatus::Complete);
    }

    #[test]
    fn an_unknown_lifecycle_is_omitted_and_makes_coverage_partial() {
        let collection = project(json!({"preferences": [
            {"memory_record_id": "odd", "key": "a", "value": "x", "memory_lifecycle": "archived"}
        ]}));
        assert!(collection.intentions.is_empty());
        assert_eq!(collection.coverage.status, CoverageStatus::Partial);
        assert_eq!(collection.coverage.omitted_ids, vec!["odd".to_string()]);
    }

    #[test]
    fn validity_windows_are_structural() {
        let collection = project(json!({"preferences": [
            {"memory_record_id": "past", "key": "a", "value": "x", "valid_until": "2026-10-01T00:00:00Z"},
            {"memory_record_id": "boundary", "key": "b", "value": "x", "valid_until": "2026-10-09T12:00:00Z"},
            {"memory_record_id": "future", "key": "c", "value": "x", "valid_until": "2026-12-01T00:00:00Z"},
            {"memory_record_id": "not-yet", "key": "d", "value": "x", "valid_from": "2027-01-01T00:00:00Z"}
        ]}));
        let ids: Vec<&str> = collection
            .intentions
            .iter()
            .map(|intention| intention.id.as_str())
            .collect();
        assert_eq!(ids, vec!["future"]);
        assert_eq!(
            find(&collection, "future").valid_until,
            Some(
                DateTime::parse_from_rfc3339("2026-12-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(collection.coverage.status, CoverageStatus::Complete);

        let unreadable = project(json!({"preferences": [
            {"memory_record_id": "garbled", "key": "a", "value": "x", "valid_until": "next tuesday"}
        ]}));
        assert!(unreadable.intentions.is_empty());
        assert_eq!(unreadable.coverage.status, CoverageStatus::Partial);
    }

    #[test]
    fn kind_comes_from_memory_kind_then_the_tier_default() {
        let collection = project(json!({
            "preferences": [
                {"memory_record_id": "unreviewed", "key": "a", "value": "x"},
                {"memory_record_id": "goal", "key": "b", "value": "x", "memory_kind": "goal"},
                {"memory_record_id": "fact", "key": "c", "value": "x", "memory_kind": "fact"},
                {"memory_record_id": "observation", "key": "d", "value": "x", "memory_kind": "observation"},
                {"memory_record_id": "inferred-instruction", "key": "e", "value": "x", "memory_kind": "instruction"},
                {"memory_record_id": "confirmed-instruction", "key": "f", "value": "x",
                 "memory_kind": "instruction", "source_type": "owner_confirmed", "confirmed_at": 1}
            ],
            "goals": [{"memory_record_id": "goal-tier", "key": "g", "value": "x"}],
            "routines": [
                {"memory_record_id": "routine", "key": "h", "value": "x"},
                {"memory_record_id": "reviewed-routine", "key": "i", "value": "x", "memory_kind": "preference"}
            ]
        }));
        assert_eq!(find(&collection, "unreviewed").kind, IntentionKind::Taste);
        assert_eq!(find(&collection, "goal").kind, IntentionKind::Goal);
        assert_eq!(find(&collection, "goal-tier").kind, IntentionKind::Goal);
        assert_eq!(
            find(&collection, "confirmed-instruction").kind,
            IntentionKind::StandingInstruction
        );
        // An instruction only Magician inferred is a taste, not a rule.
        assert_eq!(
            find(&collection, "inferred-instruction").kind,
            IntentionKind::Taste
        );
        assert_eq!(
            find(&collection, "reviewed-routine").kind,
            IntentionKind::Taste
        );
        for absent in ["fact", "observation", "routine"] {
            assert!(
                collection
                    .intentions
                    .iter()
                    .all(|intention| intention.id != absent),
                "`{absent}` is not an intention"
            );
        }
        assert_eq!(collection.coverage.status, CoverageStatus::Complete);
    }

    #[test]
    fn an_explicit_save_is_direct_only_with_owner_turn_evidence() {
        let collection = project(json!({"preferences": [
            {"memory_record_id": "model-asserted", "key": "a", "value": "x",
             "source_type": "explicit_user_statement", "memory_lifecycle": "pending_review"},
            {"memory_record_id": "assistant-quoted", "key": "b", "value": "x",
             "source_type": "explicit_user_statement",
             "memory_evidence": [{"id": "t", "source_role": "assistant", "source_id": "m", "quote": "I suggest x"}]},
            {"memory_record_id": "no-quote", "key": "c", "value": "x",
             "source_type": "correction",
             "memory_evidence": [{"id": "t", "source_role": "user", "source_id": "m"}]},
            {"memory_record_id": "owner-said", "key": "d",
             "value": "Always check with me before ordering meat for me.",
             "source_type": "explicit_user_statement", "memory_lifecycle": "pending_review",
             "memory_evidence": owner_quote()},
            {"memory_record_id": "owner-quote-other-claim", "key": "e",
             "value": "Never order anything without asking me",
             "source_type": "explicit_user_statement", "memory_lifecycle": "pending_review",
             "memory_evidence": owner_quote()}
        ]}));
        assert_eq!(
            find(&collection, "model-asserted").assertion_status,
            AssertionStatus::Inferred
        );
        assert_eq!(
            find(&collection, "assistant-quoted").assertion_status,
            AssertionStatus::Inferred
        );
        assert_eq!(
            find(&collection, "no-quote").assertion_status,
            AssertionStatus::Inferred
        );
        // A real owner quote attached to a claim it does not contain.
        assert_eq!(
            find(&collection, "owner-quote-other-claim").assertion_status,
            AssertionStatus::Inferred
        );
        let owner_said = find(&collection, "owner-said");
        assert_eq!(
            owner_said.assertion_status,
            AssertionStatus::DirectOwnerStatement
        );
        // Saved a minute ago and not reviewed yet: still governs (P0 §6.2).
        assert_eq!(owner_said.lifecycle, IntentionLifecycle::PendingReview);
        assert_eq!(
            owner_said.evidence_refs,
            vec![EvidenceRef {
                source_id: "chat-turn-1:user".to_string(),
                source_revision: None,
                role: EvidenceRole::Owner,
            }]
        );
    }

    #[test]
    fn confirmation_learning_and_capture_sources_map_to_their_force() {
        let collection = project(json!({"preferences": [
            {"memory_record_id": "confirmed", "key": "a", "value": "x",
             "source_type": "owner_confirmed", "confirmed_from": "insight", "confirmed_at": 1_700_000_000},
            {"memory_record_id": "conflict-answer", "key": "b", "value": "x",
             "source_type": "owner_confirmed", "owner_confirmed_at": "2026-10-01T00:00:00Z"},
            {"memory_record_id": "unstamped-confirmation", "key": "c", "value": "x",
             "source_type": "owner_confirmed"},
            {"memory_record_id": "learned", "key": "d", "value": "x",
             "source_type": "learning_candidate", "source_id": "cand-1",
             "learning": {"explicit_user_request": true}},
            {"memory_record_id": "meeting", "key": "e", "value": "x",
             "source_type": "meeting_capture", "memory_evidence": owner_quote()},
            {"memory_record_id": "room-origin", "key": "f", "value": "x",
             "source_type": "explicit_user_statement", "trust": "untrusted",
             "memory_evidence": owner_quote()}
        ]}));
        assert_eq!(
            find(&collection, "confirmed").assertion_status,
            AssertionStatus::Confirmed
        );
        assert_eq!(
            find(&collection, "conflict-answer").assertion_status,
            AssertionStatus::Confirmed
        );
        assert_eq!(
            find(&collection, "unstamped-confirmation").assertion_status,
            AssertionStatus::Inferred
        );
        assert_eq!(
            find(&collection, "learned").assertion_status,
            AssertionStatus::Inferred
        );
        let meeting = find(&collection, "meeting");
        assert_eq!(meeting.assertion_status, AssertionStatus::InferredUntrusted);
        assert_eq!(meeting.evidence_refs[0].role, EvidenceRole::External);
        assert_eq!(
            find(&collection, "room-origin").assertion_status,
            AssertionStatus::InferredUntrusted
        );
    }

    #[test]
    fn statements_keep_negation_conditions_and_structure() {
        let collection = project(json!({"preferences": [
            {"memory_record_id": "negated", "key": "dinner", "value": "Never order meat for me",
             "memory_context": "when ordering food delivery",
             "scope": {"topics": ["food"], "entities": [], "applies_to": []}},
            {"memory_record_id": "structured", "key": "flights",
             "value": {"seat": "aisle", "avoid": ["red-eye"], "unless": "the trip is under 2h"}},
            {"memory_record_id": "empty-scope", "key": "x", "value": "y",
             "scope": {"topics": [], "entities": [], "applies_to": []}}
        ]}));
        let negated = find(&collection, "negated");
        assert_eq!(negated.statement, "dinner: Never order meat for me");
        assert_eq!(
            negated.conditions.as_deref(),
            Some("when ordering food delivery")
        );
        assert_eq!(negated.scope.as_ref().unwrap()["topics"], json!(["food"]));
        let structured = find(&collection, "structured");
        assert!(structured.statement.starts_with("flights: "));
        assert!(structured.statement.contains("red-eye"));
        assert!(structured.statement.contains("unless"));
        assert!(find(&collection, "empty-scope").scope.is_none());
    }

    #[test]
    fn legacy_entries_get_a_stable_derived_id_and_a_content_revision() {
        let knowledge = json!({"preferences": [
            "prefers window seats",
            {"key": "tea", "value": "green, no sugar", "source_type": "inferred"}
        ]});
        let first = project(knowledge.clone());
        let second = project(json!({"preferences": [
            {"key": "tea", "value": "green, no sugar", "source_type": "inferred"},
            "prefers window seats"
        ]}));
        assert_eq!(first.intentions.len(), 2);
        assert_eq!(first.coverage.status, CoverageStatus::Complete);
        let mut first_ids: Vec<&str> = first.intentions.iter().map(|i| i.id.as_str()).collect();
        let mut second_ids: Vec<&str> = second.intentions.iter().map(|i| i.id.as_str()).collect();
        first_ids.sort_unstable();
        second_ids.sort_unstable();
        assert_eq!(first_ids, second_ids, "reordering must not change identity");
        assert!(first_ids.iter().all(|id| id.starts_with("legacy:")));
        let tea = first
            .intentions
            .iter()
            .find(|intention| intention.statement.starts_with("tea"))
            .unwrap();
        assert_eq!(
            tea.revision,
            memory_lifecycle::digest(&knowledge["preferences"][1])
        );
    }

    #[test]
    fn app_governed_and_unreadable_tiers_are_reported_not_dropped() {
        let collection = project(json!({
            "preferences": [
                {"memory_record_id": "app", "key": "a", "value": "x",
                 "app_source_eligibility": {"candidate_id": "c", "candidate_revision": "r1"}}
            ],
            "goals": {"run_a_marathon": "by spring"},
            "_memory_lifecycle": {"conflicts": {}}
        }));
        assert!(collection.intentions.is_empty());
        assert_eq!(collection.coverage.status, CoverageStatus::Partial);
        // Tier order follows the JSON map, which may preserve insertion order.
        let mut omitted = collection.coverage.omitted_ids.clone();
        omitted.sort_unstable();
        assert_eq!(omitted, vec!["app".to_string(), "tier:goals".to_string()]);
    }

    #[test]
    fn empty_knowledge_is_complete_and_a_non_object_root_is_unavailable() {
        let empty = project(json!({}));
        assert!(empty.intentions.is_empty());
        assert_eq!(empty.coverage.status, CoverageStatus::Complete);
        assert_eq!(
            project_native_knowledge_bytes(None, now()).coverage.status,
            CoverageStatus::Complete
        );

        let broken = project(json!(["not", "tiers"]));
        assert_eq!(broken.coverage.status, CoverageStatus::Unavailable);
        assert_eq!(
            broken.coverage.unavailable_sources,
            vec![NATIVE_SOURCE.to_string()]
        );
        assert_eq!(
            project_native_knowledge_bytes(Some(b"{oops"), now())
                .coverage
                .status,
            CoverageStatus::Unavailable
        );
    }
}
