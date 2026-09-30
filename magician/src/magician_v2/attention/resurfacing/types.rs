//! Core types for the Proactive Resurfacing Engine.
//!
//! These mirror the future SQLite store row and the salience-signal bundle
//! persisted as JSON. Kept provider-neutral: the corpus scanners
//! (memory/tasks/episodes/comms/calendar) all normalize into [`CorpusItem`],
//! and the scorer/store operate over [`Candidate`].

pub use crate::magician_v2::resurfacing_seam::{
    candidate_id, Candidate, CandidateState, ResurfacingChangeFact, ResurfacingContentDetails,
    ResurfacingDetailStatus, ResurfacingTemporalFact, SalienceSignals, SourceKind,
};
use std::fmt;
use std::str::FromStr;

/// Shared owner-feedback cooldowns for HTTP and background response consumers.
pub const DEFAULT_DISMISS_COOLDOWN_SECS: i64 = 21 * 86_400;
pub const DEFAULT_ACK_COOLDOWN_SECS: i64 = 60 * 86_400;

/// Owner feedback on a surfaced candidate. Drives the state/cooldown
/// transition applied by the store (see `ResurfacingStore::record_action`) and
/// is appended verbatim to the `resurfacing_feedback` log as a lowercase
/// string. Mirrors the [`CandidateState`] string-enum style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackAction {
    Open,
    Acknowledge,
    Dismiss,
    /// Worth-a-look correction: this is owner work and belongs in For you.
    OwnerWork,
}

impl FeedbackAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Acknowledge => "acknowledge",
            Self::Dismiss => "dismiss",
            Self::OwnerWork => "owner_work",
        }
    }
}

impl fmt::Display for FeedbackAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FeedbackAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" | "useful" => Ok(Self::Open),
            "acknowledge" => Ok(Self::Acknowledge),
            "dismiss" => Ok(Self::Dismiss),
            "owner_work" | "needs_me" => Ok(Self::OwnerWork),
            other => Err(format!("unknown FeedbackAction: {other}")),
        }
    }
}

/// Optional reason a "Worth a look" card was dismissed. Mirrors the
/// channel-assist dismiss vocabulary. The reason TUNES suppression:
/// `NotRelevant`/`Spam` (and a reasonless dismiss) teach the ranker to suppress
/// SIMILAR future items; the "this instance is done" reasons
/// (`AlreadyHandled`/`Duplicate`/`Delegated`) do NOT — the category is fine, only
/// this card was redundant, so we stop showing this one without penalizing its
/// neighbors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissReason {
    Spam,
    AlreadyHandled,
    Duplicate,
    Delegated,
    NotRelevant,
}

impl DismissReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::AlreadyHandled => "already_handled",
            Self::Duplicate => "duplicate",
            Self::Delegated => "delegated",
            Self::NotRelevant => "not_relevant",
        }
    }

    /// Lenient parse (trims + lowercases). Unknown/empty → `None`, so a stray
    /// value degrades to a plain reasonless dismiss rather than erroring.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "spam" => Some(Self::Spam),
            "already_handled" => Some(Self::AlreadyHandled),
            "duplicate" => Some(Self::Duplicate),
            "delegated" => Some(Self::Delegated),
            "not_relevant" => Some(Self::NotRelevant),
            _ => None,
        }
    }

    /// Whether this dismissal should teach the ranker to suppress SIMILAR future
    /// items (durable dismissed signal + negative lane engagement). "Done"
    /// reasons keep the category neutral.
    pub fn penalizes_similar(self) -> bool {
        matches!(self, Self::NotRelevant | Self::Spam)
    }
}

/// A normalized item scanned from one of the source substrates, ready to be
/// scored into a [`Candidate`]. `occurred_at` is unix seconds for salience
/// scoring; `watermark_cursor` is the source-native cursor persisted after a
/// successful scan. For most sources they are the same value, but comms can use
/// provider millisecond cursors while still scoring recency in seconds.
/// `embedding_text` is the text handed to the embedding model; `digest` is a
/// short human-facing summary.
#[derive(Debug, Clone)]
pub struct CorpusItem {
    pub source_kind: SourceKind,
    pub source_ref: String,
    pub title: String,
    pub digest: String,
    /// Optional provider-neutral, content-safe facts carried by the source.
    /// Raw source bodies must never be copied here.
    pub content_details: Option<ResurfacingContentDetails>,
    /// Source-native immutable revision for the content fields. Sources that
    /// have no revision contract leave this unset.
    pub content_revision: Option<String>,
    pub occurred_at: i64,
    pub watermark_cursor: i64,
    pub embedding_text: String,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn candidate_id_is_stable_hash_of_source() {
        let a = candidate_id(SourceKind::Memory, "user.knowledge#name");
        let b = candidate_id(SourceKind::Memory, "user.knowledge#name");
        let c = candidate_id(SourceKind::Task, "user.knowledge#name");
        assert_eq!(a, b); // stable
        assert_ne!(a, c); // source_kind participates
        assert_eq!(SourceKind::from_str("web").unwrap(), SourceKind::Web);
        assert_eq!(SourceKind::from_str("note").unwrap(), SourceKind::Note);
    }
}
