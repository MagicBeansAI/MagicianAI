//! Live Thinking Map — operation protocol (Phase 1a).
//!
//! Every change to a [`ThinkingMap`] is a bounded, validated operation carried
//! in a [`MapOperationEnvelope`] against a known `base_revision`. This module
//! defines the operation enum + envelope + their serde wire contracts ONLY.
//! The deterministic reducer that applies these (and enforces validity, the
//! model-accessible subset, timestamps, etc.) is built separately.

use serde::{Deserialize, Serialize};

use crate::magician_v2::thinking_map_models::{
    Clarification, ClarificationState, EpistemicState, MapId, MapLifecycle, NodeId, NodeKind,
    Position, PromotedRef, PromotionKind, RestructureProposal, SharedViewState, ThinkingEdge,
    ThinkingNode, THINKING_MAP_SCHEMA_VERSION,
};

/// New UUID v4 envelope id.
pub fn new_envelope_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Who is authoring an operation envelope. Internally tagged on `actor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "actor", rename_all = "snake_case")]
pub enum OperationActor {
    Owner {
        principal: String,
    },
    Participant {
        speaker_id: String,
    },
    Model {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<String>,
    },
    TrustedSystem {
        component: String,
    },
    Imported {
        source_kind: String,
    },
}

/// Pointer to the model trace/profile that produced a model-authored envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTraceRef {
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_profile: Option<String>,
}

/// A batch of operations applied atomically against `base_revision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapOperationEnvelope {
    pub schema_version: u32,
    pub envelope_id: String,
    pub map_id: MapId,
    pub base_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utterance_id: Option<String>,
    pub actor: OperationActor,
    pub idempotency_key: String,
    #[serde(default)]
    pub operations: Vec<MapOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_trace: Option<ModelTraceRef>,
    pub created_at: String,
}

impl MapOperationEnvelope {
    /// Construct an envelope with `schema_version` pinned to the current value.
    pub fn new(
        envelope_id: String,
        map_id: MapId,
        base_revision: u64,
        actor: OperationActor,
        idempotency_key: impl Into<String>,
        operations: Vec<MapOperation>,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: THINKING_MAP_SCHEMA_VERSION,
            envelope_id,
            map_id,
            base_revision,
            utterance_id: None,
            actor,
            idempotency_key: idempotency_key.into(),
            operations,
            model_trace: None,
            created_at: created_at.into(),
        }
    }
}

/// A single bounded change to a [`ThinkingMap`]. Internally tagged on `op` so
/// the wire form is e.g. `{"op":"add_node", "node": {...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MapOperation {
    /// Insert a fully-constructed node. The caller builds the `ThinkingNode`;
    /// the reducer inserts/validates it.
    AddNode {
        node: ThinkingNode,
    },
    /// Patch a node's label/detail/confidence. `detail_markdown` uses
    /// `Option<Option<String>>` so the three intents are all expressible:
    /// `None` = leave unchanged, `Some(None)` = clear to null,
    /// `Some(Some(s))` = set to `s`.
    UpdateNode {
        node_id: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail_markdown: Option<Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f32>,
    },
    SetNodeKind {
        node_id: NodeId,
        kind: NodeKind,
    },
    SetEpistemicState {
        node_id: NodeId,
        state: EpistemicState,
    },
    TombstoneNode {
        node_id: NodeId,
    },
    RestoreNode {
        node_id: NodeId,
    },
    Connect {
        edge: ThinkingEdge,
    },
    Disconnect {
        edge_id: crate::magician_v2::thinking_map_models::EdgeId,
    },
    MoveToParent {
        node_id: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<NodeId>,
    },
    MoveNode {
        node_id: NodeId,
        position: Position,
    },
    SetPositionLock {
        node_id: NodeId,
        locked: bool,
    },
    CreateClarification {
        clarification: Clarification,
    },
    ResolveClarification {
        clarification_id: crate::magician_v2::thinking_map_models::ClarificationId,
        state: ClarificationState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
    },
    ProposeRestructure {
        proposal: RestructureProposal,
    },
    ConfirmRestructure {
        proposal_id: crate::magician_v2::thinking_map_models::ProposalId,
    },
    RejectRestructure {
        proposal_id: crate::magician_v2::thinking_map_models::ProposalId,
    },
    SetSharedView {
        view_state: SharedViewState,
    },
    LinkPromotedObject {
        node_id: NodeId,
        promoted: PromotedRef,
    },
    UnlinkPromotedObject {
        node_id: NodeId,
        destination_kind: PromotionKind,
        object_id: String,
    },
    RenameSpeaker {
        old_speaker_id: String,
        new_display_name: String,
    },
    /// Owner-only: rename the whole map. Event-sourced through the reducer so it
    /// replays consistently. NOTE: `title` is not in the semantic hash, and
    /// `replay.rs` reconstructs its revision-0 base from the manifest's CURRENT
    /// title — so a historical `replay_to_sequence(N)` reflects the current
    /// title, which never causes replay divergence.
    SetTitle {
        title: String,
    },
    /// Owner-only: change the map lifecycle (active/paused/archived/deleted).
    /// Event-sourced through the reducer; `lifecycle` IS in the semantic hash,
    /// so the fold reproduces each transition exactly on replay.
    SetLifecycle {
        lifecycle: MapLifecycle,
    },
}

impl MapOperation {
    /// Whether an LLM MAY emit this operation. The reducer enforces this; this
    /// predicate just exposes the design's allowed subset.
    ///
    /// The model MAY emit content/epistemic/connection/clarification/proposal
    /// operations. It may NOT emit physical positioning
    /// (`move_node`/`set_position_lock`), proposal confirmations
    /// (`confirm_restructure`/`reject_restructure`), shared-view control
    /// (`set_shared_view`), owner-only promotion
    /// (`link_promoted_object`/`unlink_promoted_object`), speaker renames
    /// (`rename_speaker`), or owner metadata commands (`set_title`/
    /// `set_lifecycle` — a model may never rename or archive a map).
    pub fn is_model_accessible(&self) -> bool {
        matches!(
            self,
            MapOperation::AddNode { .. }
                | MapOperation::UpdateNode { .. }
                | MapOperation::SetNodeKind { .. }
                | MapOperation::SetEpistemicState { .. }
                | MapOperation::TombstoneNode { .. }
                | MapOperation::RestoreNode { .. }
                | MapOperation::Connect { .. }
                | MapOperation::Disconnect { .. }
                | MapOperation::MoveToParent { .. }
                | MapOperation::CreateClarification { .. }
                | MapOperation::ResolveClarification { .. }
                | MapOperation::ProposeRestructure { .. }
        )
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::thinking_map_models::{
        AssertionOrigin, EdgeKind, EpistemicState, ThinkingEdge, ThinkingNode,
    };
    use crate::magician_v2::thinking_map_operations::*;

    fn sample_node(id: &str) -> ThinkingNode {
        ThinkingNode {
            node_id: id.to_string(),
            kind: NodeKind::Idea,
            label: "An idea".to_string(),
            detail_markdown: None,
            epistemic_state: EpistemicState::Provisional,
            assertion_origin: AssertionOrigin::OwnerSpoken,
            confidence: 0.5,
            speaker: None,
            source_refs: vec![],
            parent_id: None,
            position: None,
            position_locked: false,
            promoted_refs: vec![],
            tombstoned: false,
            created_at: "2026-07-19T00:00:00Z".to_string(),
            updated_at: "2026-07-19T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn add_node_tag_stability() {
        let op = MapOperation::AddNode {
            node: sample_node("n1"),
        };
        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains("\"op\":\"add_node\""), "json was {json}");
    }

    #[test]
    fn other_op_tag_stability() {
        let tombstone = serde_json::to_string(&MapOperation::TombstoneNode {
            node_id: "n1".to_string(),
        })
        .unwrap();
        assert!(
            tombstone.contains("\"op\":\"tombstone_node\""),
            "json was {tombstone}"
        );

        let set_kind = serde_json::to_string(&MapOperation::SetNodeKind {
            node_id: "n1".to_string(),
            kind: NodeKind::Risk,
        })
        .unwrap();
        assert!(
            set_kind.contains("\"op\":\"set_node_kind\""),
            "json was {set_kind}"
        );

        let connect = serde_json::to_string(&MapOperation::Connect {
            edge: ThinkingEdge {
                edge_id: "e1".to_string(),
                from_node: "n1".to_string(),
                to_node: "n2".to_string(),
                kind: EdgeKind::Supports,
                assertion_origin: AssertionOrigin::ModelInferred,
                tombstoned: false,
                created_at: "2026-07-19T00:00:00Z".to_string(),
                updated_at: "2026-07-19T00:00:00Z".to_string(),
            },
        })
        .unwrap();
        assert!(connect.contains("\"op\":\"connect\""), "json was {connect}");
    }

    #[test]
    fn actor_tag_stability() {
        let owner = serde_json::to_string(&OperationActor::Owner {
            principal: "anonymous".to_string(),
        })
        .unwrap();
        assert!(owner.contains("\"actor\":\"owner\""), "json was {owner}");

        let model = serde_json::to_string(&OperationActor::Model {
            trace_id: Some("tr-1".to_string()),
        })
        .unwrap();
        assert!(model.contains("\"actor\":\"model\""), "json was {model}");
    }

    #[test]
    fn update_node_null_vs_absent_detail() {
        // Some(None) => explicit null (clear).
        let clear = MapOperation::UpdateNode {
            node_id: "n1".to_string(),
            label: None,
            detail_markdown: Some(None),
            confidence: None,
        };
        let json = serde_json::to_string(&clear).unwrap();
        assert!(
            json.contains("\"detail_markdown\":null"),
            "explicit clear must serialize null: {json}"
        );

        // None => absent from the wire (leave unchanged).
        let untouched = MapOperation::UpdateNode {
            node_id: "n1".to_string(),
            label: Some("new".to_string()),
            detail_markdown: None,
            confidence: Some(0.9),
        };
        let json = serde_json::to_string(&untouched).unwrap();
        assert!(
            !json.contains("detail_markdown"),
            "absent detail must be skipped: {json}"
        );
    }

    #[test]
    fn envelope_round_trip() {
        let env = MapOperationEnvelope::new(
            "env-1".to_string(),
            "map-1".to_string(),
            7,
            OperationActor::Owner {
                principal: "anonymous".to_string(),
            },
            "idem-1",
            vec![
                MapOperation::AddNode {
                    node: sample_node("n1"),
                },
                MapOperation::SetEpistemicState {
                    node_id: "n1".to_string(),
                    state: EpistemicState::Confirmed,
                },
            ],
            "2026-07-19T00:00:00Z",
        );
        let json = serde_json::to_string(&env).unwrap();
        let decoded: MapOperationEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, decoded);
        assert_eq!(decoded.schema_version, 1);
    }

    #[test]
    fn is_model_accessible_truth_table() {
        // Allowed subset.
        assert!(MapOperation::AddNode {
            node: sample_node("n1")
        }
        .is_model_accessible());
        assert!(MapOperation::ProposeRestructure {
            proposal: RestructureProposal {
                proposal_id: "p1".to_string(),
                proposed_by: OperationActor::Owner {
                    principal: "p".to_string(),
                },
                rationale: "r".to_string(),
                operations: vec![],
                state: crate::magician_v2::thinking_map_models::ProposalState::Proposed,
                affected_node_ids: vec![],
                created_at: "2026-07-19T00:00:00Z".to_string(),
                resolved_at: None,
            }
        }
        .is_model_accessible());

        // Disallowed subset.
        assert!(!MapOperation::MoveNode {
            node_id: "n1".to_string(),
            position: Position { x: 0.0, y: 0.0 },
        }
        .is_model_accessible());
        assert!(!MapOperation::ConfirmRestructure {
            proposal_id: "p1".to_string(),
        }
        .is_model_accessible());
        assert!(!MapOperation::SetSharedView {
            view_state: SharedViewState::default(),
        }
        .is_model_accessible());
        assert!(!MapOperation::RenameSpeaker {
            old_speaker_id: "s1".to_string(),
            new_display_name: "Name".to_string(),
        }
        .is_model_accessible());
        assert!(!MapOperation::SetPositionLock {
            node_id: "n1".to_string(),
            locked: true,
        }
        .is_model_accessible());

        // Owner metadata commands — never model-accessible.
        assert!(!MapOperation::SetTitle {
            title: "New title".to_string(),
        }
        .is_model_accessible());
        assert!(!MapOperation::SetLifecycle {
            lifecycle: crate::magician_v2::thinking_map_models::MapLifecycle::Archived,
        }
        .is_model_accessible());
    }
}
