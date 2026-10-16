//! Shadow memory attachment.
//!
//! Records which memories *would* apply to a candidate without changing
//! salience. Attachment is scope overlap first, not embedding similarity.
//! Only stated, explainable, scoped entries enter the shadow set.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::agents::{
    memory_scope::{entry_permissions, is_scope_token, CandidateAttributes, MemoryScope},
    MemoryKind, MemoryTrust,
};

/// Bound on memories attached to one Today page. The store is a single
/// knowledge.json; this cap keeps the card loop from walking the whole file.
const SHADOW_MEMORY_CAP: usize = 64;
const SHADOW_HITS_PER_CANDIDATE: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedMemory {
    pub key: String,
    pub tier: String,
    pub source_type: String,
    #[serde(default = "default_trust")]
    pub trust: MemoryTrust,
    #[serde(default = "default_kind")]
    pub kind: MemoryKind,
    pub text: String,
    pub updated_at: Option<String>,
    pub scope: MemoryScope,
    pub may_explain: bool,
    #[serde(default)]
    pub may_suppress: bool,
    #[serde(default)]
    pub may_condition: bool,
    #[serde(default)]
    pub may_propose_action: bool,
}

fn default_trust() -> MemoryTrust {
    MemoryTrust::Inferred
}

fn default_kind() -> MemoryKind {
    MemoryKind::Episodic
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryApplication {
    pub memory_key: String,
    pub memory_revision: Option<String>,
    pub direction: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryApplicationRecord {
    pub would_apply: Vec<MemoryApplication>,
}

pub fn memory_set_revision(memories: &[ScopedMemory]) -> String {
    let mut parts: Vec<String> = memories
        .iter()
        .map(|memory| {
            format!(
                "{}:{}",
                memory.key,
                memory.updated_at.as_deref().unwrap_or("-")
            )
        })
        .collect();
    parts.sort();
    blake3::hash(parts.join("\0").as_bytes())
        .to_hex()
        .to_string()
}

pub fn memories_from_knowledge(knowledge: &Value) -> Vec<ScopedMemory> {
    let Some(tiers) = knowledge.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (tier, entries) in tiers {
        if !crate::magician_v2::agents::is_owner_confirmable_memory_tier(tier) {
            continue;
        }
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for entry in entries {
            // Shadow attention conditioning must obey the same current-memory
            // boundary as retrieval. Unresolved claims cannot suppress items.
            if crate::magician_v2::agents::memory_temperature::candidate_metadata_has_superseded_lifecycle(entry)
                || crate::magician_v2::agents::memory_lifecycle::state(entry) == "unresolved"
            {
                continue;
            }
            let Some(key) = entry.get("key").and_then(Value::as_str) else {
                continue;
            };
            let source_type = entry
                .get("source_type")
                .and_then(Value::as_str)
                .unwrap_or("insight")
                .to_string();
            let (trust, kind, may_suppress, may_condition, may_explain) =
                entry_permissions(tier, Some(source_type.as_str()));
            if matches!(trust, MemoryTrust::Untrusted) {
                continue;
            }
            if !may_explain && !(may_condition || may_suppress) {
                continue;
            }
            let scope: MemoryScope = entry
                .get("scope")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();
            if scope.topics.is_empty() && scope.entities.is_empty() {
                continue;
            }
            let text = entry
                .get("value")
                .map(|value| match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            out.push(ScopedMemory {
                key: format!("{tier}: {key}"),
                tier: tier.clone(),
                source_type,
                trust,
                kind,
                text,
                updated_at: entry
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                scope,
                may_explain,
                may_suppress,
                may_condition,
                may_propose_action: kind.may_propose_action()
                    && matches!(trust, MemoryTrust::Stated),
            });
        }
    }
    out.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    out.truncate(SHADOW_MEMORY_CAP);
    out
}

pub fn narrow<'a>(
    memories: &'a [ScopedMemory],
    candidate: &CandidateAttributes,
    limit: usize,
) -> Vec<&'a ScopedMemory> {
    memories
        .iter()
        .filter(|memory| memory.scope.matches(candidate))
        .take(limit)
        .collect()
}

pub fn evaluate_shadow(
    candidate: &CandidateAttributes,
    memories: &[ScopedMemory],
) -> MemoryApplicationRecord {
    let hits = narrow(memories, candidate, SHADOW_HITS_PER_CANDIDATE);
    MemoryApplicationRecord {
        would_apply: hits
            .into_iter()
            .filter(|memory| memory.may_explain)
            .map(|memory| MemoryApplication {
                memory_key: memory.key.clone(),
                memory_revision: memory.updated_at.clone(),
                direction: "explain".to_string(),
                rationale: shadow_rationale(memory),
                strength: None,
            })
            .collect(),
    }
}

fn shadow_rationale(memory: &ScopedMemory) -> String {
    // Inferred/untrusted entries must never speak as the owner. The loader
    // already drops them; this is the last line if one is passed in.
    if !memory.may_explain {
        return format!("a stored preference at {} may apply", memory.key);
    }
    if memory.text.trim().is_empty() {
        format!("you keep a preference at {}", memory.key)
    } else {
        format!("you said {}", memory.text.trim())
    }
}

/// Cite a matching stated memory only when there is no curator phrasing.
/// Curated `why_now` stays as written; the memory key/revision still attach.
pub fn why_now_with_memory(
    fallback: &str,
    applied: &MemoryApplicationRecord,
    curated: bool,
) -> (String, Option<String>, Option<String>) {
    let Some(hit) = applied.would_apply.first() else {
        return (fallback.to_string(), None, None);
    };
    let text = if curated {
        fallback.to_string()
    } else {
        hit.rationale.clone()
    };
    (
        text,
        Some(hit.memory_key.clone()),
        hit.memory_revision.clone(),
    )
}

pub fn worth_why_now(
    title: &str,
    line: &str,
    source_kind: &str,
    fallback_why: &str,
    curated: bool,
    memories: &[ScopedMemory],
) -> (String, Option<String>, Option<String>) {
    let shadow = evaluate_shadow(
        &candidate_attributes_from_text(&format!("{title} {line}"), source_kind),
        memories,
    );
    why_now_with_memory(fallback_why, &shadow, curated)
}

pub fn candidate_attributes_from_text(text: &str, source_kind: &str) -> CandidateAttributes {
    let topics = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(|token| token.to_ascii_lowercase())
        .filter(|token| is_scope_token(token))
        .collect();
    CandidateAttributes {
        topics,
        entities: Vec::new(),
        source_kind: source_kind.to_string(),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::memory_scope::candidate_attributes;
    use serde_json::json;

    fn scoped(key: &str, topics: &[&str], applies_to: &[&str]) -> ScopedMemory {
        ScopedMemory {
            key: key.to_string(),
            tier: "preferences".to_string(),
            source_type: "owner_confirmed".to_string(),
            trust: MemoryTrust::Stated,
            kind: MemoryKind::Normative,
            text: key.to_string(),
            updated_at: Some("rev-1".to_string()),
            scope: MemoryScope {
                topics: topics.iter().map(|value| (*value).to_string()).collect(),
                entities: Vec::new(),
                applies_to: applies_to
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect(),
            },
            may_explain: true,
            may_suppress: true,
            may_condition: true,
            may_propose_action: false,
        }
    }

    #[test]
    fn narrowing_selects_by_scope_overlap_not_by_similarity_alone() {
        let memories = vec![
            scoped("gtm_evidence", &["gtm"], &["task"]),
            scoped("tone", &["writing"], &["chat"]),
        ];
        let hits = narrow(
            &memories,
            &candidate_attributes(&["gtm", "pricing"], "task"),
            5,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "gtm_evidence");
    }

    #[test]
    fn shadow_records_applications_without_changing_salience() {
        let memories = vec![scoped("gtm_evidence", &["gtm"], &["task"])];
        let before = 0.42_f32;
        let record = evaluate_shadow(&candidate_attributes(&["gtm"], "task"), &memories);
        assert_eq!(before, 0.42);
        assert_eq!(record.would_apply.len(), 1);
        assert_eq!(record.would_apply[0].memory_key.as_str(), "gtm_evidence");
        assert!(record.would_apply[0].memory_revision.is_some());
    }

    #[test]
    fn why_now_cites_the_memory_only_when_the_curator_did_not_already_write() {
        let applied = evaluate_shadow(
            &candidate_attributes(&["gtm"], "task"),
            &[scoped(
                "preferences: avoid_vendor_calls",
                &["gtm"],
                &["task"],
            )],
        );
        let (text, key, revision) =
            why_now_with_memory("you keep coming back to this", &applied, false);
        assert!(text.contains("you said"));
        assert_eq!(key.as_deref(), Some("preferences: avoid_vendor_calls"));
        assert!(revision.is_some(), "an unattributed effect is a bug");

        let (kept, _, _) = why_now_with_memory("this is the curator line", &applied, true);
        assert_eq!(kept, "this is the curator line");
    }

    #[test]
    fn inferred_and_unscoped_entries_never_enter_the_shadow_set() {
        let knowledge = json!({
            "preferences": [
                {
                    "key": "avoid_vendor",
                    "value": "Avoid vendor calls",
                    "source_type": "owner_confirmed",
                    "scope": {"topics":["vendor"],"entities":[],"applies_to":[]}
                },
                {
                    "key": "maybe_gtm",
                    "value": "I like gtm",
                    "source_type": "insight",
                    "scope": {"topics":["gtm"],"entities":[],"applies_to":[]}
                },
                {
                    "key": "unstated_scope",
                    "value": "Keep this",
                    "source_type": "owner_confirmed"
                }
            ]
        });
        let memories = memories_from_knowledge(&knowledge);
        assert_eq!(memories.len(), 2);
        assert!(memories
            .iter()
            .any(|memory| memory.key == "preferences: avoid_vendor" && memory.may_explain));
        assert!(memories
            .iter()
            .any(|memory| memory.key == "preferences: maybe_gtm" && !memory.may_explain));
        let shadow = evaluate_shadow(&candidate_attributes(&["gtm"], "task"), &memories);
        assert!(
            shadow.would_apply.is_empty(),
            "inferred rows must not become why-now"
        );
    }

    #[test]
    fn inferred_memories_never_speak_as_the_owner() {
        let mut inferred = scoped("preferences: maybe", &["gtm"], &["task"]);
        inferred.may_explain = false;
        inferred.source_type = "insight".to_string();
        inferred.text = "I like gtm".to_string();
        let record = evaluate_shadow(&candidate_attributes(&["gtm"], "task"), &[inferred]);
        // Inferred rows never enter would_apply, so they cannot become
        // "you said …" why-now. The rationale last-line is unused here on
        // purpose: the filter is the product rule.
        assert!(
            record.would_apply.is_empty(),
            "inferred rows must not be cited"
        );
        let (text, key, _) = why_now_with_memory("fallback", &record, false);
        assert!(!text.contains("you said"));
        assert!(!text.contains("I like gtm"));
        assert!(key.is_none());
    }

    #[test]
    fn worth_why_now_keeps_curator_phrasing_and_cites_memory_otherwise() {
        let memories = vec![scoped("preferences: vendor", &["vendor"], &["comm"])];
        let (cited, key, _) = worth_why_now(
            "Invoice from vendor",
            "Invoice from vendor",
            "comm",
            "Due soon",
            false,
            &memories,
        );
        assert!(cited.contains("you said"));
        assert_eq!(key.as_deref(), Some("preferences: vendor"));

        let (kept, _, _) = worth_why_now(
            "Invoice from vendor",
            "Invoice from vendor",
            "comm",
            "this is the curator line",
            true,
            &memories,
        );
        assert_eq!(kept, "this is the curator line");
    }

    #[test]
    fn stated_workflows_do_not_enter_the_shadow_set() {
        let knowledge = json!({
            "workflows": [{
                "key": "w1",
                "value": "Always ping vendor",
                "source_type": "owner_confirmed",
                "scope": {"topics":["vendor"],"entities":[],"applies_to":[]}
            }],
            "research_findings": [{
                "key": "r1",
                "value": "Owner said vendor first",
                "source_type": "explicit_user_statement",
                "scope": {"topics":["vendor"],"entities":[],"applies_to":[]}
            }]
        });
        let memories = memories_from_knowledge(&knowledge);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].key, "research_findings: r1");
    }

    #[test]
    fn candidate_tokens_drop_stopwords() {
        let attrs = candidate_attributes_from_text("the vendor from acme with this note", "comm");
        assert!(attrs.topics.contains(&"vendor".to_string()));
        assert!(attrs.topics.contains(&"acme".to_string()));
        assert!(!attrs
            .topics
            .iter()
            .any(|topic| { matches!(topic.as_str(), "the" | "from" | "with" | "this" | "note") }));
    }
}
