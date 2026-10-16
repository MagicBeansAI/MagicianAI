//! The owner's current chat turn, carried host-side to explicit memory
//! writes (P0 contracts §6.2, gap D5).
//!
//! An explicit preference ("remember: check with me before ordering meat")
//! may hold an act later only when the record proves the owner actually spoke.
//! The model's tool arguments cannot prove that, so the chat dispatcher scopes
//! the real turn — its id and a bounded excerpt of what the owner typed — in a
//! task-local around the tool call, and the writer copies it into the record's
//! `memory_evidence`. A model cannot set or read this from JSON.
//!
//! The scope is set only where the dispatcher holds the authenticated owner's
//! own message. Paths that do not (executor sub-runs, background writers) see
//! `None`, and their explicit saves read as inferred — the conservative
//! direction.

use std::future::Future;

use serde_json::{json, Value};

/// Longest excerpt kept as evidence. Enough to show the statement in context,
/// short enough that a long message does not bloat every record.
pub const MAX_OWNER_TURN_EXCERPT_CHARS: usize = 600;

/// Serializable because a run carries it across pauses and restarts
/// (`AgenticPauseState::alignment_owner_turn`), so an owner's words keep their
/// force after a resume and lose none of it to a reconstructed goal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnerTurn {
    pub turn_id: String,
    pub excerpt: String,
}

impl OwnerTurn {
    /// `None` when there is no turn id or nothing the owner said.
    pub fn new(turn_id: &str, owner_text: &str) -> Option<Self> {
        let turn_id = turn_id.trim();
        let text = owner_text.trim();
        if turn_id.is_empty() || text.is_empty() {
            return None;
        }
        let mut excerpt: String = text.chars().take(MAX_OWNER_TURN_EXCERPT_CHARS).collect();
        if text.chars().count() > MAX_OWNER_TURN_EXCERPT_CHARS {
            excerpt.push('…');
        }
        Some(Self {
            turn_id: turn_id.to_string(),
            excerpt,
        })
    }

    /// The host-issued owner span alignment checks may cite for `cancel` and
    /// `acknowledged_exception`.
    pub fn span(&self) -> super::contract::OwnerSpan {
        super::contract::OwnerSpan {
            id: format!("owner-turn:{}", self.turn_id),
            text: self.excerpt.clone(),
        }
    }

    /// The `memory_evidence` entry for this turn. The id names the turn, so
    /// every save made in one turn counts as one observation.
    pub fn evidence(&self) -> Value {
        json!({
            "id": format!("chat-turn:{}", self.turn_id),
            "source_role": "user",
            "source_id": self.turn_id,
            "source_type": "explicit_user_statement",
            "quote": self.excerpt,
        })
    }
}

tokio::task_local! {
    static CURRENT_OWNER_TURN: Option<OwnerTurn>;
}

/// The owner turn in scope for the current tool call, if the dispatcher set
/// one.
pub fn current_owner_turn() -> Option<OwnerTurn> {
    CURRENT_OWNER_TURN.try_with(Clone::clone).ok().flatten()
}

/// Run `future` with `turn` in scope. Inline-await only: a `tokio::spawn`
/// inside drops the scope, which degrades to "no owner turn", never to a
/// forged one.
///
/// `future` is held by value inside the scope, so pass a large dispatch
/// future boxed (`Box::pin(..)`) to keep the caller's state machine small.
pub async fn with_owner_turn<F: Future>(turn: Option<OwnerTurn>, future: F) -> F::Output {
    CURRENT_OWNER_TURN.scope(turn, future).await
}

/// Words too common to ground a claim. Negations are kept on purpose:
/// "never order meat" is not grounded by "order meat".
const GROUNDING_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "are", "was", "from", "have", "has", "any",
    "all", "our", "its", "into", "about", "when", "then", "than", "them", "they", "his", "her",
    "she", "him", "will", "would", "should", "can", "could", "just", "also", "very", "some",
    "more", "most", "such", "only", "own", "same", "too", "each", "both", "few", "other",
    "what", "which", "who", "whom", "whose", "why", "how", "where", "there", "here", "been",
    "being", "were", "does", "did", "doing", "having", "please", "remember", "save", "note",
    "you", "your", "yours", "mine", "myself", "user", "owner", "preference", "prefers",
];

/// Share of a claim's content words the owner's text must contain.
const GROUNDING_SHARE_PERCENT: usize = 60;

fn content_words(text: &str) -> Vec<String> {
    let mut words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|word| word.trim_matches('\''))
        .filter(|word| word.chars().count() >= 3 && !GROUNDING_STOPWORDS.contains(word))
        .map(str::to_string)
        .collect();
    words.sort();
    words.dedup();
    words
}

fn same_word(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // "ordering" and "order", "weekends" and "weekend".
    let shared = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let shorter = a.chars().count().min(b.chars().count());
    shared >= 5 || (shared >= 4 && shared == shorter)
}

/// Whether the owner's own words (`quote`) ground `claim`: most of the
/// claim's content words appear in what the owner said. A real owner quote
/// attached to a claim the owner never made — a save a page told the model
/// to make during the owner's turn, or a distilled "fact" citing an unrelated
/// line — is not grounded, and the record reads as inferred.
pub fn quote_grounds_claim(quote: &str, claim: &str) -> bool {
    // A negation flips the claim: it must be the owner's own.
    let raw = |text: &str| -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !(c.is_alphanumeric() || c == '\''))
            .map(|word| word.trim_matches('\'').replace('\'', ""))
            .filter(|word| !word.is_empty())
            .collect()
    };
    let (claim_raw, quote_raw) = (raw(claim), raw(quote));
    let negated = |words: &[String]| {
        words.iter().any(|word| {
            matches!(
                word.as_str(),
                "no" | "not" | "never" | "dont" | "doesnt" | "without" | "avoid" | "stop" | "nothing"
                    | "cant" | "wont" | "isnt" | "arent" | "nor"
            )
        })
    };
    if negated(&claim_raw) && !negated(&quote_raw) {
        return false;
    }
    let claim_words = content_words(claim);
    if claim_words.is_empty() {
        return false;
    }
    let quote_words = content_words(quote);
    let present = claim_words
        .iter()
        .filter(|word| quote_words.iter().any(|said| same_word(word, said)))
        .count();
    present * 100 >= claim_words.len() * GROUNDING_SHARE_PERCENT
}

/// Whether the owner's words name any content word of `claim`.
pub fn quote_mentions_claim(quote: &str, claim: &str) -> bool {
    let quote_words = content_words(quote);
    content_words(claim)
        .iter()
        .any(|word| quote_words.iter().any(|said| same_word(word, said)))
}

/// Whether a saved preference may be cleared or deleted by the current
/// call. One that may hold acts (the owner's direct statement or a
/// confirmed one) needs the owner's own turn in scope, naming it: a model
/// talked into "forget the meat rule" by a page cannot remove what the owner
/// asked for. Anything else may go as before.
pub fn owner_turn_may_retire(item: &Value) -> bool {
    use super::intention::AssertionStatus;
    match super::sources::native::assertion_status(item) {
        AssertionStatus::DirectOwnerStatement | AssertionStatus::Confirmed => {
            let claim = format!(
                "{} {}",
                item.get("key").and_then(Value::as_str).unwrap_or_default(),
                item.get("value").map(claim_text).unwrap_or_default()
            );
            current_owner_turn().is_some_and(|turn| quote_mentions_claim(&turn.excerpt, &claim))
        },
        _ => true,
    }
}

/// The text a saved value claims, for [`quote_grounds_claim`].
pub fn claim_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owners_own_words_ground_a_claim() {
        let said = "Remember: check with me before ordering meat for me.";
        assert!(quote_grounds_claim(said, "Check with me before ordering meat for me."));
        assert!(quote_grounds_claim("I'm vegetarian, remember that", "vegetarian"));
        // A page told the model to save this during an unrelated turn.
        assert!(!quote_grounds_claim(
            "summarise this page",
            "Never order anything without asking me"
        ));
        // A negation the owner never said is not grounded.
        assert!(!quote_grounds_claim("order meat on fridays", "never order meat"));
        assert!(!quote_grounds_claim(said, ""));
    }

    #[test]
    fn an_empty_turn_is_no_turn() {
        assert!(OwnerTurn::new("", "remember this").is_none());
        assert!(OwnerTurn::new("t1", "   ").is_none());
    }

    #[test]
    fn the_excerpt_is_bounded() {
        let long = "x".repeat(MAX_OWNER_TURN_EXCERPT_CHARS + 50);
        let turn = OwnerTurn::new("t1", &long).expect("turn");
        assert_eq!(
            turn.excerpt.chars().count(),
            MAX_OWNER_TURN_EXCERPT_CHARS + 1
        );
    }

    #[test]
    fn evidence_is_owner_role_with_a_turn_identity() {
        let turn =
            OwnerTurn::new("t1", "Always check with me before booking weekends.").expect("turn");
        let evidence = turn.evidence();
        assert_eq!(evidence["source_role"], "user");
        assert_eq!(evidence["source_id"], "t1");
        assert_eq!(evidence["id"], "chat-turn:t1");
        assert_eq!(
            evidence["quote"],
            "Always check with me before booking weekends."
        );
    }

    #[tokio::test]
    async fn the_turn_is_visible_only_inside_its_scope() {
        assert!(current_owner_turn().is_none());
        let turn = OwnerTurn::new("t1", "hi");
        let seen = with_owner_turn(turn.clone(), async { current_owner_turn() }).await;
        assert_eq!(seen, turn);
        assert!(current_owner_turn().is_none());
    }
}
