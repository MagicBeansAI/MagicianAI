//! Episodic memory as index candidates.
//!
//! Episodes live on their own disk surface (`memory/agents/<id>/episodes/*.json`,
//! one file per episode) and never pass through the tier walk in
//! [`crate::memory_candidates`], so before this module they were the one
//! candidate producer the index had never seen. A chat turn still assembled
//! them at query time, but with no indexed row to score against they fell to
//! keyword ranking while every tier candidate got BM25 + dense vector fusion.
//!
//! # Why the conversion lives here and not in magician
//!
//! `V3EpisodeRecord` is magician's type, and this crate cannot depend on
//! magician. It can, however, read the JSON those records are written as —
//! the same arrangement [`crate::memory_candidates::load_app_memory_index_projection`]
//! already uses for app-contributed memory.
//!
//! That direction matters for correctness, not just layering. A candidate's
//! index identity is derived from `scope`, `agent_id`, `goal_id`, `tier_name`
//! and `item_key`, and its score key additionally hashes
//! `semantic_memory_type` and `content_hash` — and `content_hash` is a hash of
//! the composed search text. So a second implementation of that text, living
//! on the magician side, would silently produce a different score key: the
//! indexed row and the runtime candidate would stop agreeing, every episode
//! would score zero, and nothing would report an error. One builder, used by
//! both the index build and the query path, is what removes that failure mode
//! rather than documenting it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs;

use crate::memory_candidates::{MemoryCandidateDocument, SemanticMemoryType};
use crate::memory_tiers::TierScope;
use crate::retrieval_scope::{stamp_engagement_scope, ContextLabel};
use crate::storage_trait::{MemoryStorage, MemoryStorageError};

/// The tier name every episode candidate carries.
///
/// Episodes are not a declared memory tier — no agent definition lists them —
/// but the candidate contract is keyed by tier name, so they need one. It is
/// also the value the `tier` filter on `search_memory` matches.
pub const EPISODE_TIER_NAME: &str = "episodes";

/// Read-only projection of magician's on-disk `V3EpisodeRecord`.
///
/// Only the fields that compose the search text, the candidate identity, or
/// the metadata are named. Everything else on the record is ignored, and every
/// field is `#[serde(default)]` so an older or newer record shape loads rather
/// than failing the whole index build — a missing field costs recall on one
/// episode, a hard error would cost the entire rebuild.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeCandidateProjectionV1 {
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub episode_id: String,
    #[serde(default)]
    pub goal_key: String,
    #[serde(default)]
    pub trigger_seq: u64,
    #[serde(default)]
    pub completed_at: String,
    #[serde(default)]
    pub outcome_kind: String,
    #[serde(default)]
    pub outcome_summary: String,
    #[serde(default)]
    pub outcome_remaining: Option<String>,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub task_description: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub strategy_summary: Option<String>,
    #[serde(default)]
    pub context_at_start: Option<String>,
    #[serde(default)]
    pub observations: Vec<String>,
    /// The occasion the episode was produced in, server-minted at write time.
    /// `None` for every owner-surface turn.
    #[serde(default)]
    pub origin_meeting: Option<String>,
}

impl EpisodeCandidateProjectionV1 {
    /// The containment label this one episode asserts.
    ///
    /// A room-produced episode is readable from that same room and nowhere
    /// else; an owner-surface episode names no occasion and resolves to
    /// [`ContextLabel::Unlabelled`], which a bound retrieval refuses. Blank and
    /// whitespace-only ids are treated as absent, so a blank cannot become an
    /// occasion a room might match on.
    pub fn origin_meeting_label(&self) -> ContextLabel {
        match self
            .origin_meeting
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            Some(meeting) => ContextLabel::Meeting(meeting.to_string()),
            None => ContextLabel::Unlabelled,
        }
    }
}

/// Flatten an episode into the single text blob the index and the keyword
/// scorer both read.
///
/// Concatenates every text-bearing field worth weighting, whitespace
/// normalised, empty fields skipped. An episode that composes to nothing
/// returns empty, which the downstream `score == 0` filter drops.
pub fn compose_episode_search_text(episode: &EpisodeCandidateProjectionV1) -> String {
    let observations_joined = episode.observations.join("\n");
    let mut parts: Vec<&str> = Vec::new();
    parts.push(episode.outcome_summary.as_str());
    if let Some(remaining) = episode.outcome_remaining.as_deref() {
        parts.push(remaining);
    }
    if let Some(title) = episode.task_title.as_deref() {
        parts.push(title);
    }
    if let Some(desc) = episode.task_description.as_deref() {
        parts.push(desc);
    }
    if let Some(strategy) = episode.strategy_summary.as_deref() {
        parts.push(strategy);
    }
    if let Some(context) = episode.context_at_start.as_deref() {
        parts.push(context);
    }
    if !observations_joined.is_empty() {
        parts.push(observations_joined.as_str());
    }
    parts
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The metadata object an episode candidate carries.
///
/// `candidate_kind` reuses the key the tier-candidate path emits so downstream
/// consumers can branch on one field name across both surfaces.
///
/// The containment label is stamped through the canonical writer. An owner
/// episode is written as an explicit null rather than omitted, which keeps the
/// distinction between "this pipeline looked and found no label" and "this
/// pipeline never looked" — the first is refused under a bound retrieval, the
/// second would be a silent pass.
pub fn episode_candidate_metadata(episode: &EpisodeCandidateProjectionV1) -> Value {
    let mut metadata = json!({
        "candidate_kind": "episode",
        "semantic_memory_type": SemanticMemoryType::Episode.as_str(),
        "episode_id": episode.episode_id.as_str(),
        "goal_key": episode.goal_key.as_str(),
        "trigger_seq": episode.trigger_seq,
        "outcome_kind": episode.outcome_kind.as_str(),
        "outcome_summary_preview": truncate_for_audit(&episode.outcome_summary, 240),
        "task_title": episode.task_title.as_deref(),
        "task_id": episode.task_id.as_deref(),
        "completed_at": episode.completed_at.as_str(),
    });
    stamp_engagement_scope(&mut metadata, &episode.origin_meeting_label());
    metadata
}

/// Build the candidate document for one episode.
///
/// `source_path` is the episode's file. `json_pointer` is deliberately empty:
/// the retrieval target is the whole episode file, not a field inside it, and
/// `forget_memory` reads that empty pointer as the whole-file marker.
pub fn episode_candidate_document(
    agent_id: &str,
    source_path: PathBuf,
    episode: EpisodeCandidateProjectionV1,
) -> MemoryCandidateDocument {
    let text = compose_episode_search_text(&episode);
    let content_hash = format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex());
    let last_updated = DateTime::parse_from_rfc3339(&episode.completed_at)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let metadata = episode_candidate_metadata(&episode);
    MemoryCandidateDocument {
        principal: episode.principal,
        workspace: episode.workspace,
        agent_id: Some(agent_id.to_string()),
        scope: TierScope::Agent,
        tier_name: EPISODE_TIER_NAME.to_string(),
        semantic_memory_type: SemanticMemoryType::Episode,
        goal_id: Some(episode.goal_key),
        item_key: episode.episode_id,
        source_path: Some(source_path),
        json_pointer: String::new(),
        content_hash,
        last_updated,
        confidence: None,
        text,
        metadata_json: metadata,
    }
}

/// Every episode on disk for one agent, as index candidates.
///
/// A missing episodes directory is an agent that has recorded none, which is
/// an empty result rather than an error. An individual file that cannot be
/// read or parsed is skipped: one malformed episode must not fail a whole
/// index rebuild, and the cost of skipping it is that one episode keeps
/// keyword-only ranking.
pub async fn load_episode_candidate_documents(
    storage: &dyn MemoryStorage,
    agent_id: &str,
) -> Result<Vec<MemoryCandidateDocument>, MemoryStorageError> {
    let episodes_dir = match storage.agent_episodes_dir(agent_id) {
        Ok(dir) => dir,
        Err(_) => return Ok(Vec::new()),
    };
    let mut entries = match fs::read_dir(&episodes_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(MemoryStorageError::Io(error)),
    };

    let mut candidates = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(MemoryStorageError::Io)? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(projection) = read_episode_projection(storage, &path, agent_id).await else {
            continue;
        };
        candidates.push(episode_candidate_document(agent_id, path, projection));
    }
    Ok(candidates)
}

/// Read one episode file, applying the same integrity checks the runtime's own
/// loader applies before it will hand an episode back.
///
/// The directory an episode sits in is not proof of whose it is. A restored
/// backup or a hand-copied file can carry a different `agent_id` than the
/// directory implies, and indexing it under the directory's agent would let a
/// search scoped to one agent retrieve another agent's episodic record — which
/// `validate_loaded_native_episode` refuses at runtime, so the index must
/// refuse it too rather than becoming the softer path to the same content.
///
/// A blank `goal_key` is rejected for the same reason: the runtime will not
/// list such a record, so indexing one creates a row that can be scored and
/// returned but never appears in the canonical load.
async fn read_episode_projection(
    storage: &dyn MemoryStorage,
    path: &Path,
    expected_agent_id: &str,
) -> Option<EpisodeCandidateProjectionV1> {
    let value = storage.read_json_value(path).await.ok()?;
    let projection: EpisodeCandidateProjectionV1 = serde_json::from_value(value).ok()?;
    projection_is_indexable(&projection, expected_agent_id).then_some(projection)
}

/// Whether one parsed episode may be indexed under `expected_agent_id`.
///
/// Pure so the rule is testable without standing up a storage impl, and so the
/// three refusals are stated in one place rather than inline in a read loop.
fn projection_is_indexable(
    projection: &EpisodeCandidateProjectionV1,
    expected_agent_id: &str,
) -> bool {
    // An episode with no identity cannot be keyed, and a keyless candidate
    // would collide with every other keyless one under `item_key`.
    !projection.episode_id.trim().is_empty()
        && projection.agent_id.trim() == expected_agent_id.trim()
        && !projection.goal_key.trim().is_empty()
}

/// One-line, ellipsised preview for the audit metadata.
///
/// Mirrors magician's `truncate_text_for_audit`, which is what produced this
/// field before the builder moved here: whitespace is collapsed to single
/// spaces so a multi-line episode summary stays one readable line, the length
/// is measured on the collapsed text, and an elision is marked. An earlier
/// version of this function trimmed only, which silently changed the preview
/// for every episode into a multi-line fragment with no marker.
///
/// The crate cannot call magician's copy — the dependency runs the other way —
/// so this is a deliberate mirror rather than a reuse. It is not
/// `truncate_projection_text`, which trims without collapsing and is correct
/// for its own single-line inputs.
fn truncate_for_audit(text: &str, max_chars: usize) -> String {
    let single_line: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let collapsed = single_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(max_chars).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval_scope::{label_from_metadata, RetrievalScope};

    fn projection() -> EpisodeCandidateProjectionV1 {
        EpisodeCandidateProjectionV1 {
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            agent_id: "envoy".to_string(),
            episode_id: "ep-1".to_string(),
            goal_key: "task_abc".to_string(),
            trigger_seq: 3,
            completed_at: "2026-09-02T10:00:00Z".to_string(),
            outcome_kind: "goal_achieved".to_string(),
            outcome_summary: "  shipped the fix  ".to_string(),
            outcome_remaining: None,
            task_title: Some("ship the fix".to_string()),
            task_description: None,
            task_id: Some("task_abc".to_string()),
            strategy_summary: None,
            context_at_start: None,
            observations: vec!["borrow checker complained".to_string(), String::new()],
            origin_meeting: None,
        }
    }

    #[test]
    fn search_text_skips_empty_parts_and_trims() {
        let text = compose_episode_search_text(&projection());
        assert_eq!(
            text,
            "shipped the fix\nship the fix\nborrow checker complained"
        );
    }

    #[test]
    fn an_episode_with_no_text_composes_to_empty() {
        let mut empty = projection();
        empty.outcome_summary = "   ".to_string();
        empty.task_title = None;
        empty.observations = Vec::new();
        assert!(compose_episode_search_text(&empty).is_empty());
    }

    #[test]
    fn a_room_episode_is_labelled_to_that_meeting() {
        let mut room = projection();
        room.origin_meeting = Some("  meeting-a  ".to_string());
        let metadata = episode_candidate_metadata(&room);
        assert_eq!(
            label_from_metadata(&metadata),
            ContextLabel::Meeting("meeting-a".to_string())
        );
        assert!(RetrievalScope::for_meeting("meeting-a")
            .expect("named meeting")
            .admits_metadata(&metadata));
        assert!(!RetrievalScope::for_meeting("meeting-b")
            .expect("named meeting")
            .admits_metadata(&metadata));
    }

    #[test]
    fn an_owner_episode_is_an_explicit_null_and_unreadable_from_a_room() {
        let metadata = episode_candidate_metadata(&projection());
        assert!(
            metadata.get("engagement_scope").is_some_and(Value::is_null),
            "the label must be an explicit null, not omitted"
        );
        assert_eq!(label_from_metadata(&metadata), ContextLabel::Unlabelled);
        assert!(!RetrievalScope::for_meeting("meeting-a")
            .expect("named meeting")
            .admits_metadata(&metadata));
        assert!(RetrievalScope::Unbound.admits_metadata(&metadata));
    }

    #[test]
    fn a_blank_meeting_id_is_absent_not_a_label() {
        for blank in ["", "   ", "\t"] {
            let mut record = projection();
            record.origin_meeting = Some(blank.to_string());
            assert_eq!(record.origin_meeting_label(), ContextLabel::Unlabelled);
        }
    }

    #[test]
    fn the_document_carries_the_identity_the_index_keys_on() {
        let document =
            episode_candidate_document("envoy", PathBuf::from("/tmp/ep-1.json"), projection());
        assert_eq!(document.tier_name, EPISODE_TIER_NAME);
        assert_eq!(document.agent_id.as_deref(), Some("envoy"));
        assert_eq!(document.goal_id.as_deref(), Some("task_abc"));
        assert_eq!(document.item_key, "ep-1");
        assert!(matches!(document.scope, TierScope::Agent));
        assert!(matches!(
            document.semantic_memory_type,
            SemanticMemoryType::Episode
        ));
        assert!(
            document.json_pointer.is_empty(),
            "the retrieval target is the whole episode file"
        );
        assert!(document.content_hash.starts_with("blake3:"));
    }

    /// The directory an episode sits in is not proof of whose it is. Indexing a
    /// misfiled record under the directory's agent would let a search scoped to
    /// one agent read another agent's episodic record — which the runtime
    /// loader refuses, so the index must refuse it too rather than becoming the
    /// softer path to the same content.
    #[test]
    fn only_the_agents_own_listable_episodes_are_indexable() {
        let mine = EpisodeCandidateProjectionV1 {
            agent_id: "envoy".to_string(),
            episode_id: "ep-1".to_string(),
            goal_key: "g".to_string(),
            ..Default::default()
        };
        assert!(projection_is_indexable(&mine, "envoy"));

        let theirs = EpisodeCandidateProjectionV1 {
            agent_id: "cto".to_string(),
            ..mine.clone()
        };
        assert!(
            !projection_is_indexable(&theirs, "envoy"),
            "another agent's record must not be indexed under this agent"
        );

        // The runtime will not list a record with a blank goal, so indexing one
        // creates a row that can be scored and returned but never loaded.
        let blank_goal = EpisodeCandidateProjectionV1 {
            goal_key: "   ".to_string(),
            ..mine.clone()
        };
        assert!(!projection_is_indexable(&blank_goal, "envoy"));

        let blank_id = EpisodeCandidateProjectionV1 {
            episode_id: "  ".to_string(),
            ..mine.clone()
        };
        assert!(!projection_is_indexable(&blank_id, "envoy"));
    }

    #[test]
    fn the_audit_preview_is_one_line_and_marks_elision() {
        let mut long = projection();
        long.outcome_summary = format!("first line\n\n  second   line  {}", "x".repeat(400));
        let metadata = episode_candidate_metadata(&long);
        let preview = metadata
            .get("outcome_summary_preview")
            .and_then(Value::as_str)
            .expect("preview");
        assert!(
            !preview.contains('\n'),
            "a multi-line summary must collapse to one line: {preview:?}"
        );
        assert!(preview.contains("first line second line"), "{preview:?}");
        assert!(preview.ends_with('…'), "an elided preview must say so");
    }

    #[test]
    fn an_unknown_field_does_not_fail_the_projection() {
        // Records gain fields; a rebuild must not fail because one arrived.
        let value = json!({
            "episode_id": "ep-2",
            "goal_key": "g",
            "outcome_summary": "did it",
            "a_field_added_later": {"nested": true},
        });
        let projection: EpisodeCandidateProjectionV1 =
            serde_json::from_value(value).expect("unknown fields are ignored");
        assert_eq!(projection.episode_id, "ep-2");
        assert_eq!(projection.trigger_seq, 0);
    }
}
