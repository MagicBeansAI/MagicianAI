//! Per-entry applicability scope.
//!
//! Extracted at write time and shown back as editable. An unscoped or
//! malformed extraction matches nothing — empty scope must never read as
//! "applies to everything".

use serde::{Deserialize, Serialize};

use super::memory_provenance::{MemoryKind, MemoryTrust};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryScope {
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub applies_to: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateAttributes {
    pub topics: Vec<String>,
    pub entities: Vec<String>,
    pub source_kind: String,
}

impl MemoryScope {
    pub fn parse(raw: &str) -> Option<Self> {
        let parsed: Self = serde_json::from_str(raw).ok()?;
        if !parsed.topics.iter().all(|value| !value.trim().is_empty())
            || !parsed.entities.iter().all(|value| !value.trim().is_empty())
            || !parsed
                .applies_to
                .iter()
                .all(|value| !value.trim().is_empty())
        {
            return None;
        }
        Some(parsed)
    }

    pub fn is_empty(&self) -> bool {
        self.topics.is_empty() && self.entities.is_empty() && self.applies_to.is_empty()
    }

    pub fn matches(&self, candidate: &CandidateAttributes) -> bool {
        if self.is_empty() {
            return false;
        }
        // Kind-only scopes would match every comm/web/task card. Topics or
        // entities are required so a preference cannot become a global bias.
        if self.topics.is_empty() && self.entities.is_empty() {
            return false;
        }
        let topic_ok = self.topics.is_empty()
            || self.topics.iter().all(|topic| {
                candidate
                    .topics
                    .iter()
                    .any(|candidate_topic| candidate_topic.eq_ignore_ascii_case(topic))
            });
        let entity_ok = self.entities.is_empty()
            || self.entities.iter().any(|entity| {
                candidate
                    .entities
                    .iter()
                    .any(|candidate_entity| candidate_entity.eq_ignore_ascii_case(entity))
                    || candidate
                        .topics
                        .iter()
                        .any(|candidate_topic| candidate_topic.eq_ignore_ascii_case(entity))
            });
        let kind_ok = self.applies_to.is_empty()
            || self
                .applies_to
                .iter()
                .any(|kind| candidate.source_kind.eq_ignore_ascii_case(kind));
        topic_ok && entity_ok && kind_ok
    }
}

const SCOPE_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "this", "that", "have", "has", "had", "you", "your",
    "are", "was", "were", "been", "being", "into", "about", "over", "under", "after", "before",
    "than", "then", "them", "they", "their", "just", "more", "some", "such", "only", "also",
    "when", "what", "which", "will", "would", "could", "should", "not", "but", "its",
];

const GENERIC_SCOPE_VERBS: &[&str] = &[
    "avoid", "keep", "make", "use", "get", "set", "need", "want", "like", "prefer", "call",
    "calls", "note", "notes", "item", "items", "data", "info",
];

/// Content-bearing token used for both extraction and candidate matching.
/// Stopwords and 1–2 character fragments never become scope or card tokens.
pub fn is_scope_token(token: &str) -> bool {
    let token = token.trim();
    token.len() >= 3
        && token.chars().all(|ch| ch.is_ascii_alphanumeric())
        && !SCOPE_STOPWORDS.contains(&token)
        && !GENERIC_SCOPE_VERBS.contains(&token)
}

pub fn extract_scope_from_text(key: &str, value: &str) -> Option<MemoryScope> {
    let mut topics = Vec::new();
    for raw in key
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .chain(value.split(|ch: char| !ch.is_ascii_alphanumeric()))
    {
        let token = raw.trim().to_ascii_lowercase();
        if !is_scope_token(&token) {
            continue;
        }
        if !topics.iter().any(|existing: &String| existing == &token) {
            topics.push(token);
        }
        if topics.len() == 8 {
            break;
        }
    }
    if topics.is_empty() {
        return None;
    }
    Some(MemoryScope {
        topics,
        entities: Vec::new(),
        applies_to: Vec::new(),
    })
}

pub fn candidate_attributes(topics: &[&str], source_kind: &str) -> CandidateAttributes {
    CandidateAttributes {
        topics: topics.iter().map(|value| (*value).to_string()).collect(),
        entities: Vec::new(),
        source_kind: source_kind.to_string(),
    }
}

/// Tiers the owner may confirm and that shadow attach may cite.
pub const OWNER_CONFIRMABLE_MEMORY_TIERS: &[&str] = &["preferences", "research_findings"];

pub fn is_owner_confirmable_memory_tier(tier: &str) -> bool {
    OWNER_CONFIRMABLE_MEMORY_TIERS
        .iter()
        .any(|name| *name == tier)
}

/// Trust, kind, and whether this entry may suppress or condition salience.
pub fn entry_permissions(
    tier: &str,
    source_type: Option<&str>,
) -> (MemoryTrust, MemoryKind, bool, bool, bool) {
    let trust = MemoryTrust::from_source_type(source_type.unwrap_or("insight"));
    let kind = MemoryKind::for_tier(tier);
    let may_explain = matches!(trust, MemoryTrust::Stated) && kind.may_explain();
    (
        trust,
        kind,
        trust.may_suppress(kind),
        trust.may_condition_salience() && kind.may_condition_salience(),
        may_explain,
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn scope_parses_and_fails_closed_on_malformed_output() {
        let good = r#"{"topics":["gtm","pricing"],"applies_to":["task","web"],"entities":[]}"#;
        let scope = MemoryScope::parse(good).unwrap();
        assert_eq!(scope.topics, vec!["gtm", "pricing"]);

        assert!(MemoryScope::parse("not json").is_none());
        assert!(MemoryScope::parse(r#"{"topics":"gtm"}"#).is_none());
    }

    #[test]
    fn an_unscoped_memory_matches_nothing_until_scope_is_supplied() {
        let unscoped = MemoryScope::default();
        assert!(!unscoped.matches(&candidate_attributes(&["gtm"], "task")));
    }

    #[test]
    fn every_owner_topic_must_appear_on_the_card() {
        let both = MemoryScope {
            topics: vec!["gtm".to_string(), "pricing".to_string()],
            entities: Vec::new(),
            applies_to: Vec::new(),
        };
        assert!(both.matches(&candidate_attributes(&["gtm", "pricing"], "task")));
        assert!(!both.matches(&candidate_attributes(&["gtm"], "task")));
    }

    #[test]
    fn a_kind_only_scope_does_not_match_every_card_in_that_lane() {
        let comm_only = MemoryScope {
            topics: Vec::new(),
            entities: Vec::new(),
            applies_to: vec!["comm".to_string()],
        };
        assert!(!comm_only.matches(&candidate_attributes(&["invoice", "acme"], "comm")));
    }

    #[test]
    fn extracted_scope_drops_stopwords_and_short_tokens() {
        let scope = extract_scope_from_text("avoid_vendor_calls", "the vendor from acme").unwrap();
        assert!(scope.topics.contains(&"vendor".to_string()));
        assert!(scope.topics.contains(&"acme".to_string()));
        assert!(!scope
            .topics
            .iter()
            .any(|topic| { matches!(topic.as_str(), "the" | "from" | "avoid" | "calls") }));
    }

    #[test]
    fn an_entity_scope_matches_the_named_entity_or_the_same_token_on_the_card() {
        let acme = MemoryScope {
            topics: Vec::new(),
            entities: vec!["acme".to_string()],
            applies_to: Vec::new(),
        };
        let mut named = candidate_attributes(&[], "comm");
        named.entities = vec!["acme".to_string()];
        assert!(acme.matches(&named));
        assert!(acme.matches(&candidate_attributes(&["invoice", "acme"], "comm")));
        assert!(!acme.matches(&candidate_attributes(&["invoice"], "comm")));
    }

    #[test]
    fn three_letter_content_tokens_survive_extraction() {
        let scope = extract_scope_from_text("gtm_notes", "keep gtm evidence").unwrap();
        assert!(scope.topics.contains(&"gtm".to_string()));
    }
}
