//! Live Thinking Map — domain model (Phase 1a).
//!
//! Pure, dormant typed core for the Live Thinking Map: finalized speech
//! continuously updates a *typed, revisable* model of the user's thinking.
//! Ideas/facts/questions/decisions/risks/actions/metrics/groups are
//! first-class nodes; later speech can correct/supersede/reconnect earlier
//! nodes; every change is a bounded, validated operation against a known
//! revision.
//!
//! This module contains ONLY the data types + their serde wire contracts.
//! The deterministic reducer that applies operations (and injects timestamps)
//! is built separately and does not live here. Nothing in this module calls
//! `Utc::now()` — timestamps are `String` (RFC3339) injected by the reducer.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::magician_v2::thinking_map_operations::{MapOperation, OperationActor};

// ── ID type aliases ─────────────────────────────────────────────────────────
// The crate does not use newtype wrappers for ids (see artifact_v2 which uses
// bare `String`). We type-alias for readability only.

pub type NodeId = String;
pub type EdgeId = String;
pub type ClarificationId = String;
pub type ProposalId = String;
pub type MapId = String;

// ── ID constructor helpers ──────────────────────────────────────────────────

/// New UUID v4 node id.
pub fn new_node_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// New UUID v4 edge id.
pub fn new_edge_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// New UUID v4 clarification id.
pub fn new_clarification_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// New UUID v4 proposal id.
pub fn new_proposal_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// New UUID v4 map id.
pub fn new_map_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ── Enums ────────────────────────────────────────────────────────────────────

/// First-class kinds of thinking node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Idea,
    Fact,
    Question,
    Decision,
    Option,
    Risk,
    Action,
    Metric,
    Assumption,
    Evidence,
    Group,
}

/// Epistemic status of a node — how settled/true it currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicState {
    Provisional,
    Asserted,
    Confirmed,
    Contradicted,
    Rejected,
    Resolved,
    Superseded,
}

/// Where an assertion (node/edge) originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionOrigin {
    OwnerSpoken,
    ParticipantSpoken,
    OwnerEdited,
    ImportedSource,
    ModelInferred,
    SystemDerived,
}

/// Directed relationship kinds between nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    RelatedTo,
    Supports,
    Contradicts,
    Answers,
    DependsOn,
    LeadsTo,
    AlternativeTo,
    Measures,
    GroupedUnder,
}

/// Map lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MapLifecycle {
    #[default]
    Active,
    Paused,
    Archived,
    Deleted,
}

/// Origin of the thinking map — where the finalized speech comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThinkingMapSource {
    Solo,
    Meeting { thread_id: String },
    Observe { session_id: String },
    Chat { thread_id: String },
    Imported { source_kind: String },
    Tutor { lesson_id: String },
}

/// Which lens the shared view renders the map through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ViewLens {
    #[default]
    Graph,
    MindMap,
    Outline,
    Decision,
    Metrics,
}

/// Destination kind for a node promoted to another Magician surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionKind {
    Task,
    Today,
    Memory,
}

/// Lifecycle of a clarification request against a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClarificationState {
    #[default]
    Open,
    Answered,
    Deferred,
    Dismissed,
}

/// Lifecycle of a restructure proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    #[default]
    Proposed,
    Confirmed,
    Rejected,
    Deferred,
}

// ── Supporting structs ───────────────────────────────────────────────────────

/// Shared view state broadcast to all viewers of the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SharedViewState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_node: Option<NodeId>,
    #[serde(default)]
    pub lens: ViewLens,
}

/// Reference to a speaker who authored an utterance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerRef {
    pub speaker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// A citation back into finalized speech / a source. All fields optional so a
/// node may cite many partial references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SourceRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utterance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Physical position of a node on the shared canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// Record of a node promoted into another Magician surface (task/today/memory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotedRef {
    pub destination_kind: PromotionKind,
    pub object_id: String,
    pub linked_at: String,
}

/// A clarification question raised against a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clarification {
    pub clarification_id: ClarificationId,
    pub node_id: NodeId,
    pub question: String,
    #[serde(default)]
    pub state: ClarificationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
}

/// Fail-closed default proposer (the most-restricted actor) used only when
/// deserializing a proposal that omits `proposed_by`. The reducer always
/// overwrites `proposed_by` from the proposing envelope's actor at propose
/// time, so this default only guards malformed/legacy input.
fn default_proposed_by() -> OperationActor {
    OperationActor::Imported {
        source_kind: "unknown".to_string(),
    }
}

/// A proposed structural rewrite of the map (a bundle of operations the owner
/// can confirm/reject as a unit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestructureProposal {
    pub proposal_id: ProposalId,
    /// Server-authoritative record of who proposed this restructure. Stamped by
    /// the reducer from the proposing envelope's actor at propose time (any
    /// client-supplied value is overwritten). On confirm, the proposal's inner
    /// operations are re-authorized and applied under THIS actor's authority —
    /// never blanket owner authority — so a proposal can never grant its inner
    /// ops more power than its author held.
    #[serde(default = "default_proposed_by")]
    pub proposed_by: OperationActor,
    pub rationale: String,
    #[serde(default)]
    pub operations: Vec<MapOperation>,
    #[serde(default)]
    pub state: ProposalState,
    #[serde(default)]
    pub affected_node_ids: Vec<NodeId>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
}

// ── Core graph elements ──────────────────────────────────────────────────────

/// A first-class node in the thinking map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingNode {
    pub node_id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_markdown: Option<String>,
    pub epistemic_state: EpistemicState,
    pub assertion_origin: AssertionOrigin,
    /// 0.0..=1.0 (range validated by the reducer, not here).
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<SpeakerRef>,
    #[serde(default)]
    pub source_refs: Vec<SourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(default)]
    pub position_locked: bool,
    #[serde(default)]
    pub promoted_refs: Vec<PromotedRef>,
    /// Tombstone flag. The reducer sets this instead of deleting so history is
    /// preserved; a tombstoned node is hidden from live views.
    #[serde(default)]
    pub tombstoned: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A record of an envelope the reducer has already applied. Used purely for
/// idempotent replay detection (matched by `envelope_id` OR `idempotency_key`);
/// EXCLUDED from the semantic hash because it is bookkeeping, not board content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedEnvelopeRecord {
    pub envelope_id: String,
    pub idempotency_key: String,
    pub resulting_revision: u64,
}

/// Maximum number of applied-envelope records retained (most-recent-first
/// eviction from the front). Bounds the idempotency ledger.
pub const APPLIED_ENVELOPE_LEDGER_CAP: usize = 512;

/// A directed edge between two nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingEdge {
    pub edge_id: EdgeId,
    pub from_node: NodeId,
    pub to_node: NodeId,
    pub kind: EdgeKind,
    pub assertion_origin: AssertionOrigin,
    /// Tombstone flag — see [`ThinkingNode::tombstoned`].
    #[serde(default)]
    pub tombstoned: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// The full Live Thinking Map document — the canonical typed core the reducer
/// applies operations against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingMap {
    pub schema_version: u32,
    pub map_id: MapId,
    pub principal: String,
    pub workspace: String,
    pub title: String,
    pub source: ThinkingMapSource,
    #[serde(default)]
    pub lifecycle: MapLifecycle,
    pub revision: u64,
    #[serde(default)]
    pub view_state: SharedViewState,
    #[serde(default)]
    pub nodes: BTreeMap<NodeId, ThinkingNode>,
    #[serde(default)]
    pub edges: BTreeMap<EdgeId, ThinkingEdge>,
    #[serde(default)]
    pub clarifications: BTreeMap<ClarificationId, Clarification>,
    #[serde(default)]
    pub proposals: BTreeMap<ProposalId, RestructureProposal>,
    /// Idempotency ledger of already-applied envelopes (bounded to the most
    /// recent [`APPLIED_ENVELOPE_LEDGER_CAP`] records). Bookkeeping only:
    /// EXCLUDED from the semantic hash.
    #[serde(default)]
    pub applied_envelopes: VecDeque<AppliedEnvelopeRecord>,
    pub created_at: String,
    pub updated_at: String,
}

/// Current schema version for `ThinkingMap` / `MapOperationEnvelope`.
pub const THINKING_MAP_SCHEMA_VERSION: u32 = 1;

impl ThinkingMap {
    /// Construct an empty map at revision 0, lifecycle `Active`, default view
    /// state. `created_at`/`updated_at` are both set to the supplied RFC3339
    /// timestamp (the reducer owns timestamp generation).
    pub fn new(
        map_id: MapId,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        title: impl Into<String>,
        source: ThinkingMapSource,
        created_at: impl Into<String>,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            schema_version: THINKING_MAP_SCHEMA_VERSION,
            map_id,
            principal: principal.into(),
            workspace: workspace.into(),
            title: title.into(),
            source,
            lifecycle: MapLifecycle::Active,
            revision: 0,
            view_state: SharedViewState::default(),
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
            clarifications: BTreeMap::new(),
            proposals: BTreeMap::new(),
            applied_envelopes: VecDeque::new(),
            updated_at: created_at.clone(),
            created_at,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn sample_node(id: &str) -> ThinkingNode {
        ThinkingNode {
            node_id: id.to_string(),
            kind: NodeKind::Decision,
            label: "Ship v1".to_string(),
            detail_markdown: Some("**decide** to ship".to_string()),
            epistemic_state: EpistemicState::Asserted,
            assertion_origin: AssertionOrigin::OwnerSpoken,
            confidence: 0.8,
            speaker: Some(SpeakerRef {
                speaker_id: "spk-1".to_string(),
                display_name: Some("Alex".to_string()),
            }),
            source_refs: vec![SourceRef {
                utterance_id: Some("utt-1".to_string()),
                thread_id: None,
                quote: Some("let's ship".to_string()),
                timestamp: Some("2026-07-19T00:00:00Z".to_string()),
            }],
            parent_id: None,
            position: Some(Position { x: 1.0, y: 2.5 }),
            position_locked: false,
            promoted_refs: vec![PromotedRef {
                destination_kind: PromotionKind::Task,
                object_id: "task-1".to_string(),
                linked_at: "2026-07-19T00:00:00Z".to_string(),
            }],
            tombstoned: false,
            created_at: "2026-07-19T00:00:00Z".to_string(),
            updated_at: "2026-07-19T00:00:00Z".to_string(),
        }
    }

    fn sample_edge(id: &str, from: &str, to: &str) -> ThinkingEdge {
        ThinkingEdge {
            edge_id: id.to_string(),
            from_node: from.to_string(),
            to_node: to.to_string(),
            kind: EdgeKind::Supports,
            assertion_origin: AssertionOrigin::ModelInferred,
            tombstoned: false,
            created_at: "2026-07-19T00:00:00Z".to_string(),
            updated_at: "2026-07-19T00:00:00Z".to_string(),
        }
    }

    fn populated_map() -> ThinkingMap {
        let mut map = ThinkingMap::new(
            "map-1".to_string(),
            "anonymous",
            "default",
            "Design chat",
            ThinkingMapSource::Meeting {
                thread_id: "thread-9".to_string(),
            },
            "2026-07-19T00:00:00Z",
        );
        map.revision = 3;
        map.view_state = SharedViewState {
            active_node: Some("n1".to_string()),
            lens: ViewLens::Outline,
        };
        map.nodes.insert("n1".to_string(), sample_node("n1"));
        map.nodes.insert("n2".to_string(), sample_node("n2"));
        map.edges
            .insert("e1".to_string(), sample_edge("e1", "n1", "n2"));
        map.clarifications.insert(
            "c1".to_string(),
            Clarification {
                clarification_id: "c1".to_string(),
                node_id: "n1".to_string(),
                question: "Which platform first?".to_string(),
                state: ClarificationState::Open,
                answer: None,
                created_at: "2026-07-19T00:00:00Z".to_string(),
                resolved_at: None,
            },
        );
        map.proposals.insert(
            "p1".to_string(),
            RestructureProposal {
                proposal_id: "p1".to_string(),
                proposed_by: OperationActor::Owner {
                    principal: "anonymous".to_string(),
                },
                rationale: "Group the risks".to_string(),
                operations: vec![],
                state: ProposalState::Proposed,
                affected_node_ids: vec!["n1".to_string(), "n2".to_string()],
                created_at: "2026-07-19T00:00:00Z".to_string(),
                resolved_at: None,
            },
        );
        map
    }

    #[test]
    fn thinking_map_serde_round_trip() {
        let map = populated_map();
        let json = serde_json::to_string(&map).expect("serialize");
        let decoded: ThinkingMap = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(map, decoded);
    }

    #[test]
    fn new_map_defaults() {
        let map = ThinkingMap::new(
            "m".to_string(),
            "p",
            "w",
            "t",
            ThinkingMapSource::Solo,
            "2026-07-19T00:00:00Z",
        );
        assert_eq!(map.schema_version, 1);
        assert_eq!(map.revision, 0);
        assert_eq!(map.lifecycle, MapLifecycle::Active);
        assert!(map.nodes.is_empty());
        assert!(map.edges.is_empty());
        assert_eq!(map.view_state.lens, ViewLens::Graph);
        assert_eq!(map.created_at, map.updated_at);
    }

    #[test]
    fn source_tag_stability() {
        let json = serde_json::to_string(&ThinkingMapSource::Meeting {
            thread_id: "t".to_string(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"meeting\""), "json was {json}");

        let solo = serde_json::to_string(&ThinkingMapSource::Solo).unwrap();
        assert!(solo.contains("\"kind\":\"solo\""), "json was {solo}");
    }

    #[test]
    fn enum_snake_case() {
        assert_eq!(
            serde_json::to_string(&NodeKind::Decision).unwrap(),
            "\"decision\""
        );
        assert_eq!(
            serde_json::to_string(&EpistemicState::Superseded).unwrap(),
            "\"superseded\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::AlternativeTo).unwrap(),
            "\"alternative_to\""
        );
        assert_eq!(
            serde_json::to_string(&AssertionOrigin::OwnerSpoken).unwrap(),
            "\"owner_spoken\""
        );
        assert_eq!(
            serde_json::to_string(&ViewLens::MindMap).unwrap(),
            "\"mind_map\""
        );
        assert_eq!(
            serde_json::to_string(&PromotionKind::Today).unwrap(),
            "\"today\""
        );
    }

    #[test]
    fn defaults_resolve() {
        assert_eq!(MapLifecycle::default(), MapLifecycle::Active);
        assert_eq!(ViewLens::default(), ViewLens::Graph);
        assert_eq!(ClarificationState::default(), ClarificationState::Open);
        assert_eq!(ProposalState::default(), ProposalState::Proposed);
    }

    #[test]
    fn id_helpers_are_uuid_v4() {
        let id = new_node_id();
        assert_eq!(id.len(), 36, "uuid v4 hyphenated is 36 chars: {id}");
        assert_ne!(new_edge_id(), new_edge_id());
    }
}
