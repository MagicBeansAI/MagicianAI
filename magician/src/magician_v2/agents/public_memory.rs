//! Memory a public agent may keep and show.
//!
//! The store is the boundary. A fully isolated agent reads and writes
//! `agents/{id}/memory/user_memory/`, never the shared user root. A public
//! surface then admits only notes labelled for a public audience and the one
//! meeting in progress. An unlabelled note is not a public note.
//!
//! The text filter strips identifier-shaped secrets on the way out and refuses
//! them on the way in. It does not try to recognise a name beside a school.
//! That kind of fact must not be written in the first place.

use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use regex::Regex;
use serde_json::{json, Map, Value};

use super::{
    memory::AgentMemoryService,
    retrieval_scope::{label_from_metadata, ContextLabel, RetrievalScope, PUBLIC_MEMORY_TOKEN},
    storage::{AgentStorage, AgentStorageError},
    types::UserMemoryIsolation,
    MemoryCandidateDocument, SemanticMemoryType,
};

/// Directory a fully isolated agent uses for user-scoped notes.
/// `None` when the agent shares the owner's user root.
pub fn isolated_user_memory_root(
    service: &AgentMemoryService,
    agent_id: &str,
    isolation: &UserMemoryIsolation,
) -> Option<PathBuf> {
    if *isolation != UserMemoryIsolation::FullyIsolated {
        return None;
    }
    service.user_memory_root(agent_id, isolation).ok()
}

/// Remove identifier-shaped text. Dates (`2026-10-04`) and money (`$42`) stay.
pub fn redact_public_surface_text(value: &str) -> String {
    let without_emails = email_pattern().replace_all(value, "[email omitted]");
    let without_jwts = jwt_pattern().replace_all(&without_emails, "[secret omitted]");
    let without_secrets = secret_pattern().replace_all(&without_jwts, "$1 [secret omitted]");
    let without_labels =
        labeled_identifier_pattern().replace_all(&without_secrets, |caps: &regex::Captures<'_>| {
            let raw = caps.get(2).map(|value| value.as_str()).unwrap_or_default();
            if identifier_like(raw) {
                format!("{} [identifier omitted]", &caps[1])
            } else {
                caps[0].to_string()
            }
        });
    let without_plus = plus_phone_pattern().replace_all(&without_labels, "[identifier omitted]");
    redact_long_digit_spans(&without_plus)
}

/// True when redaction would change the text. A public write refuses that
/// value instead of storing a secret and hoping a later read strips it.
pub fn value_contains_personal_identifier(value: &str) -> bool {
    redact_public_surface_text(value) != value
}

/// Prompt line after redaction. An empty line is withheld.
pub fn public_prompt_line(value: &str) -> Option<String> {
    let redacted = redact_public_surface_text(value);
    let trimmed = redacted.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Whether this candidate may be shown on a public surface.
///
/// [`RetrievalScope::PublicDurable`] stays strict, and so does this function.
/// No label is not a public label. Automatic notes (insights, archive, task
/// progress) stay hidden until something labels them for the public audience
/// or for this meeting. People, places, and procedures stay hidden either way.
pub fn public_surface_shows_candidate(
    scope: &RetrievalScope,
    candidate: &MemoryCandidateDocument,
) -> bool {
    let RetrievalScope::PublicDurable { meeting_id } = scope else {
        return false;
    };
    if withheld_public_kind(candidate) {
        return false;
    }
    match label_from_metadata(&candidate.metadata_json) {
        ContextLabel::Public => !is_episode(candidate),
        ContextLabel::Meeting(item_meeting) => meeting_id.as_deref() == Some(item_meeting.as_str()),
        ContextLabel::Unlabelled | ContextLabel::Neutral | ContextLabel::Engagement(_) => false,
    }
}

/// Prompt admission. Public surfaces use [`public_surface_shows_candidate`].
/// Every other scope keeps the strict label rule.
pub fn prompt_candidate_admitted(
    scope: &RetrievalScope,
    candidate: &MemoryCandidateDocument,
) -> bool {
    match scope {
        RetrievalScope::PublicDurable { .. } => public_surface_shows_candidate(scope, candidate),
        other => other.admits_metadata(&candidate.metadata_json),
    }
}

/// Write one preference into the agent's own user-memory file.
///
/// The returned `scope` is `isolated_user`, so the owner lifecycle pass that
/// runs for `scope == "user"` does not touch the shared root.
pub async fn save_isolated_public_preference(
    service: &AgentMemoryService,
    agent_id: &str,
    tier_name: &str,
    key: &str,
    value: &Value,
) -> Value {
    let mut fields = Map::new();
    fields.insert(key.to_string(), value.clone());
    merge_isolated_public_user_fields(service, agent_id, tier_name, &fields).await
}

/// Merge fields into the isolated knowledge file and stamp each item `public`.
pub async fn merge_isolated_public_user_fields(
    service: &AgentMemoryService,
    agent_id: &str,
    tier_name: &str,
    fields: &Map<String, Value>,
) -> Value {
    if tier_name.split('.').any(|part| part.starts_with('_')) || tier_name.is_empty() {
        return json!({
            "status": "error",
            "reason": "Private memory metadata is not a writable memory tier",
        });
    }
    if isolated_tier_is_private(tier_name) {
        return json!({
            "status": "error",
            "reason": "save refused: people, contacts, and places stay off a public agent's notes",
        });
    }
    if let Some(reason) = personal_identifier_in_fields(fields) {
        return json!({
            "status": "error",
            "reason": reason,
        });
    }
    let root = match service.user_memory_root(agent_id, &UserMemoryIsolation::FullyIsolated) {
        Ok(root) => root,
        Err(error) => {
            return json!({
                "status": "error",
                "reason": format!("isolated memory root failed: {error}"),
            });
        },
    };
    let path = root.join("knowledge.json");
    if fields_are_only_clears(fields) && !path.exists() {
        return json!({
            "status": "ok",
            "scope": "isolated_user",
            "tier": tier_name,
            "retracted": 0,
        });
    }
    if let Err(error) = service.storage().create_dir_all(&root).await {
        return json!({
            "status": "error",
            "reason": format!("isolated memory directory failed: {error}"),
        });
    }
    let _guard = match AgentStorage::acquire_file_lock_exclusive(&path).await {
        Ok(guard) => guard,
        Err(error) => {
            return json!({
                "status": "error",
                "reason": format!("isolated memory lock failed: {error}"),
            });
        },
    };
    let mut knowledge = match read_object(service.storage(), &path).await {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "status": "error",
                "reason": format!("isolated memory load failed: {error}"),
            });
        },
    };
    let mut items = knowledge
        .get(tier_name)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut updated = Vec::new();
    let mut retracted = 0usize;
    for (key, value) in fields {
        if value.as_str() == Some("") {
            let before = items.len();
            items.retain(|item| !item_key_is(item, key));
            retracted += before.saturating_sub(items.len());
            continue;
        }
        let item = stamped_public_item(key, value);
        if let Some(existing) = items.iter_mut().find(|item| item_key_is(item, key)) {
            *existing = item;
        } else {
            items.push(item);
        }
        updated.push(key.clone());
    }
    if let Some(object) = knowledge.as_object_mut() {
        object.insert(tier_name.to_string(), Value::Array(items));
    }
    if let Err(error) = service.storage().write_json_atomic(&path, &knowledge).await {
        return json!({
            "status": "error",
            "reason": format!("isolated memory save failed: {error}"),
        });
    }
    service.mark_index_dirty("isolated_public_memory");
    json!({
        "status": "ok",
        "scope": "isolated_user",
        "tier": tier_name,
        "fields_updated": updated,
        "retracted": retracted,
    })
}

fn withheld_public_kind(candidate: &MemoryCandidateDocument) -> bool {
    if matches!(
        candidate.semantic_memory_type,
        SemanticMemoryType::Entity
            | SemanticMemoryType::Social
            | SemanticMemoryType::Environment
            | SemanticMemoryType::Procedure
    ) {
        return true;
    }
    let tier = candidate.tier_name.to_ascii_lowercase();
    tier == "entities" || tier == "contacts" || tier.starts_with("environment_knowledge")
}

fn is_episode(candidate: &MemoryCandidateDocument) -> bool {
    candidate.semantic_memory_type == SemanticMemoryType::Episode
        || candidate.tier_name.eq_ignore_ascii_case("episodes")
        || candidate
            .metadata_json
            .get("candidate_kind")
            .and_then(Value::as_str)
            == Some("episode")
}

fn personal_identifier_in_fields(fields: &Map<String, Value>) -> Option<String> {
    for (key, value) in fields {
        if value.as_str() == Some("") {
            continue;
        }
        if value_contains_personal_identifier(key)
            || value_contains_personal_identifier(&field_text(value))
        {
            return Some(
                "save refused: a public note cannot contain a name-shaped identifier, \
                 phone number, email, address, account, or secret"
                    .to_string(),
            );
        }
    }
    None
}

fn field_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("value")
            .map(field_text)
            .unwrap_or_else(|| value.to_string()),
        other => other.to_string(),
    }
}

fn isolated_tier_is_private(tier_name: &str) -> bool {
    let leaf = tier_name
        .rsplit(['.', '/'])
        .next()
        .unwrap_or(tier_name)
        .to_ascii_lowercase();
    leaf == "entities"
        || leaf == "contacts"
        || leaf == "environment"
        || leaf.starts_with("environment_knowledge")
        || leaf.contains("contact")
}

fn fields_are_only_clears(fields: &Map<String, Value>) -> bool {
    !fields.is_empty() && fields.values().all(|value| value.as_str() == Some(""))
}

fn item_key_is(item: &Value, key: &str) -> bool {
    item.get("key")
        .and_then(Value::as_str)
        .is_some_and(|found| found.eq_ignore_ascii_case(key))
}

fn stamped_public_item(key: &str, value: &Value) -> Value {
    let mut item = if let Some(object) = value.as_object() {
        object.clone()
    } else {
        Map::from_iter([
            ("key".to_string(), Value::String(key.to_string())),
            ("value".to_string(), value.clone()),
            ("confidence".to_string(), json!(1.0)),
            (
                "source_type".to_string(),
                Value::String("explicit_user_statement".to_string()),
            ),
        ])
    };
    item.entry("key".to_string())
        .or_insert_with(|| Value::String(key.to_string()));
    item.insert(
        "updated_at".to_string(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    item.insert(
        "engagement_scope".to_string(),
        Value::String(PUBLIC_MEMORY_TOKEN.to_string()),
    );
    Value::Object(item)
}

async fn read_object(storage: &AgentStorage, path: &Path) -> Result<Value, AgentStorageError> {
    match storage.read_json::<Value>(path).await {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Ok(Value::Object(Map::new())),
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(Map::new()))
        },
        Err(error) => Err(error),
    }
}

fn identifier_like(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.chars().any(|ch| ch.is_ascii_digit())
        || trimmed.chars().count() >= 16
        || (trimmed.chars().count() >= 4
            && trimmed
                .chars()
                .filter(|ch| ch.is_ascii_alphabetic())
                .all(|ch| ch.is_ascii_uppercase())
            && trimmed.chars().any(|ch| ch.is_ascii_alphabetic()))
}

fn email_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}").expect("email pattern")
    });
    &PATTERN
}

fn jwt_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}")
            .expect("jwt pattern")
    });
    &PATTERN
}

fn secret_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(api[ _-]?key|access[ _-]?token|token|password|secret)\b\s*[:=]\s*\S+")
            .expect("secret pattern")
    });
    &PATTERN
}

fn labeled_identifier_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(ssn|passport|account|acct|card|phone|mobile|email|address)\b\s*[:=#]?\s*(\+?\d[\d\- ]{3,}\d|[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}|[A-Z0-9]{6,})",
        )
        .expect("labeled identifier pattern")
    });
    &PATTERN
}

fn plus_phone_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\+\d[\d\-\s]{8,}\d").expect("phone pattern"));
    &PATTERN
}

fn redact_long_digit_spans(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        let start = index;
        let (end, digits) = collect_digit_span(&chars, start);
        let original: String = chars[start..end].iter().collect();
        if looks_like_date(&original) || looks_like_money(&chars, start) {
            out.push_str(&original);
        } else if digits.len() >= 10
            || (digits.len() >= 5 && identifier_context(&chars, start, end))
        {
            out.push_str("[identifier omitted]");
        } else {
            out.push_str(&original);
        }
        index = end;
    }
    out
}

fn collect_digit_span(chars: &[char], start: usize) -> (usize, String) {
    let mut index = start;
    let mut digits = String::new();
    loop {
        while index < chars.len() && chars[index].is_ascii_digit() {
            digits.push(chars[index]);
            index += 1;
        }
        if index + 1 < chars.len()
            && matches!(chars[index], ' ' | '-')
            && chars[index + 1].is_ascii_digit()
        {
            index += 1;
            continue;
        }
        break;
    }
    (index, digits)
}

fn looks_like_date(original: &str) -> bool {
    let bytes = original.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && original
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

fn looks_like_money(chars: &[char], start: usize) -> bool {
    start > 0 && chars[start - 1] == '$'
}

fn identifier_context(chars: &[char], start: usize, end: usize) -> bool {
    let from = start.saturating_sub(24);
    let to = (end + 24).min(chars.len());
    let context: String = chars[from..to].iter().collect();
    let context = context.to_ascii_lowercase();
    [
        "ssn", "passport", "account", "acct", "card", "phone", "mobile", "email", "address",
    ]
    .iter()
    .any(|word| context.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::agents::TierScope;
    use serde_json::json;

    fn candidate(
        scope: TierScope,
        tier: &str,
        kind: SemanticMemoryType,
        label: Option<&str>,
        text: &str,
    ) -> MemoryCandidateDocument {
        let mut metadata = json!({
            "candidate_kind": if kind == SemanticMemoryType::Episode { "episode" } else { "tier_field" },
        });
        if let Some(label) = label {
            metadata["engagement_scope"] = json!(label);
        }
        MemoryCandidateDocument {
            principal: None,
            workspace: None,
            agent_id: Some("envoy".to_string()),
            scope,
            tier_name: tier.to_string(),
            semantic_memory_type: kind,
            goal_id: None,
            item_key: "note".to_string(),
            source_path: None,
            json_pointer: "/preferences/0".to_string(),
            content_hash: "hash".to_string(),
            last_updated: chrono::Utc::now(),
            confidence: Some(1.0),
            text: text.to_string(),
            metadata_json: metadata,
        }
    }

    #[test]
    fn redaction_strips_identifiers_and_keeps_dates_and_money() {
        let text = redact_public_surface_text(
            "Mail ada@example.com about the 2026-10-04 note. Budget $42. Phone 415-555-1212. token=abc123secret",
        );
        assert!(!text.contains("ada@example.com"));
        assert!(text.contains("2026-10-04"));
        assert!(text.contains("$42"));
        assert!(!text.contains("415-555-1212"));
        assert!(!text.contains("abc123secret"));
        assert!(!value_contains_personal_identifier(
            "likes short answers about rust"
        ));
        assert!(value_contains_personal_identifier(
            "reach me at ada@example.com"
        ));
    }

    #[test]
    fn public_surface_keeps_public_notes_and_this_meeting_and_drops_unlabelled_notes() {
        let open = RetrievalScope::public_durable(None);
        let mine = RetrievalScope::public_durable(Some("meeting-1".to_string()));
        let insight = candidate(
            TierScope::Agent,
            "insights",
            SemanticMemoryType::AgentContext,
            None,
            "answer in short sentences",
        );
        assert!(!public_surface_shows_candidate(&open, &insight));
        assert!(!public_surface_shows_candidate(&mine, &insight));
        let archive = candidate(
            TierScope::Agent,
            "archive",
            SemanticMemoryType::SourceEvidence,
            None,
            "an older conversation",
        );
        assert!(!public_surface_shows_candidate(&open, &archive));
        let task = candidate(
            TierScope::AgentGoal,
            "task_progress",
            SemanticMemoryType::ProjectContext,
            None,
            "the assistant reply from another chat",
        );
        assert!(!public_surface_shows_candidate(&open, &task));
        assert!(!public_surface_shows_candidate(&mine, &task));
        let preference = candidate(
            TierScope::User,
            "knowledge.preferences",
            SemanticMemoryType::UserPreference,
            None,
            "owner prefers mornings",
        );
        assert!(!public_surface_shows_candidate(&open, &preference));
        let public_preference = candidate(
            TierScope::User,
            "knowledge.preferences",
            SemanticMemoryType::UserPreference,
            Some("public"),
            "likes short answers",
        );
        assert!(public_surface_shows_candidate(&open, &public_preference));
        let episode = candidate(
            TierScope::Agent,
            "episodes",
            SemanticMemoryType::Episode,
            None,
            "talked about a school",
        );
        assert!(!public_surface_shows_candidate(&open, &episode));
        let entity = candidate(
            TierScope::Agent,
            "entities",
            SemanticMemoryType::Entity,
            None,
            "a person",
        );
        assert!(!public_surface_shows_candidate(&open, &entity));
        let this_meeting = candidate(
            TierScope::Agent,
            "episodes",
            SemanticMemoryType::Episode,
            Some("meeting:meeting-1"),
            "this room",
        );
        assert!(public_surface_shows_candidate(&mine, &this_meeting));
        assert!(!public_surface_shows_candidate(&open, &this_meeting));
        let other_meeting = candidate(
            TierScope::Agent,
            "episodes",
            SemanticMemoryType::Episode,
            Some("meeting:meeting-2"),
            "another room",
        );
        assert!(!public_surface_shows_candidate(&mine, &other_meeting));
        assert!(!mine.admits(&ContextLabel::Unlabelled));
    }

    #[tokio::test]
    async fn isolated_preference_write_stays_out_of_the_shared_root() {
        let temp = tempfile::tempdir().expect("temp root");
        let service = AgentMemoryService::with_base_path(temp.path());
        let saved = save_isolated_public_preference(
            &service,
            "envoy",
            "preferences",
            "brevity",
            &json!("likes short answers"),
        )
        .await;
        assert_eq!(saved.get("status").and_then(Value::as_str), Some("ok"));
        assert_eq!(
            saved.get("scope").and_then(Value::as_str),
            Some("isolated_user")
        );
        let isolated = temp
            .path()
            .join("agents/envoy/memory/user_memory/knowledge.json");
        let body = std::fs::read_to_string(&isolated).expect("isolated knowledge");
        assert!(body.contains("likes short answers"));
        assert!(body.contains("\"engagement_scope\": \"public\""));
        assert!(!service.storage().user_knowledge_path().exists());

        let refused = save_isolated_public_preference(
            &service,
            "envoy",
            "preferences",
            "contact",
            &json!("ada@example.com"),
        )
        .await;
        assert_eq!(refused.get("status").and_then(Value::as_str), Some("error"));
        let body = std::fs::read_to_string(&isolated).expect("isolated knowledge unchanged");
        assert!(!body.contains("ada@example.com"));
        assert!(!service.storage().user_knowledge_path().exists());

        let contacts = save_isolated_public_preference(
            &service,
            "other",
            "contacts",
            "note",
            &json!("the lobby is on the third floor"),
        )
        .await;
        assert_eq!(
            contacts.get("status").and_then(Value::as_str),
            Some("error")
        );
        assert!(!temp.path().join("agents/other").exists());
    }
}
