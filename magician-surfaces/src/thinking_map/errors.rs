//! Live Thinking Map — reducer error taxonomy (Phase 1b).
//!
//! Every variant carries ONLY ids, enum/tag names, and fixed reason codes —
//! never raw model/user text or `model_trace` contents. This keeps errors safe
//! to log/surface even when an operation originated from an untrusted source.

/// Result alias for reducer/validation operations.
pub type ThinkingMapResult<T> = Result<T, ThinkingMapError>;

/// Errors returned when applying a [`super::operations::MapOperationEnvelope`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ThinkingMapError {
    /// The envelope's `base_revision` did not match the map's current revision.
    #[error("revision conflict: expected {expected}, got {actual}")]
    RevisionConflict { expected: u64, actual: u64 },

    /// Operation targeted a node id that is not present in the map.
    #[error("unknown node: {0}")]
    UnknownNode(String),

    /// Operation targeted an edge id that is not present in the map.
    #[error("unknown edge: {0}")]
    UnknownEdge(String),

    /// Operation targeted a clarification id that is not present in the map.
    #[error("unknown clarification: {0}")]
    UnknownClarification(String),

    /// Operation targeted a proposal id that is not present in the map.
    #[error("unknown proposal: {0}")]
    UnknownProposal(String),

    /// `add_node` for a node id already present (tombstoned or not).
    #[error("duplicate node: {0}")]
    DuplicateNode(String),

    /// `connect` for an edge id already present.
    #[error("duplicate edge: {0}")]
    DuplicateEdge(String),

    /// `create_clarification` for a clarification id already present.
    #[error("duplicate clarification: {0}")]
    DuplicateClarification(String),

    /// `propose_restructure` for a proposal id already present.
    #[error("duplicate proposal: {0}")]
    DuplicateProposal(String),

    /// The acting actor is not permitted to emit this operation. `reason` is a
    /// fixed reason code (e.g. `actor_not_permitted`, `confirm_reserved_for_owner`).
    #[error("authority violation: actor={actor} op={op} reason={reason}")]
    AuthorityViolation {
        actor: String,
        op: String,
        reason: String,
    },

    /// The claimed `assertion_origin` on a created node/edge does not match the
    /// acting actor.
    #[error("origin mismatch: expected {expected}, got {actual}")]
    OriginMismatch { expected: String, actual: String },

    /// A `confidence` value fell outside `0.0..=1.0`.
    #[error("confidence out of range: {0}")]
    ConfidenceOutOfRange(f32),

    /// Adding this edge would create a cycle within a cycle-forbidden edge kind.
    #[error("cycle detected in edge kind: {edge_kind}")]
    CycleDetected { edge_kind: String },

    /// `move_to_parent` would make a node its own ancestor.
    #[error("parent cycle")]
    ParentCycle,

    /// A promoted object `(destination_kind, object_id)` is already linked to a
    /// different node.
    #[error("promotion link conflict: kind={destination_kind} object={object_id}")]
    PromotionLinkConflict {
        destination_kind: String,
        object_id: String,
    },

    /// A catch-all for invariant violations expressed as fixed reason codes
    /// (e.g. `empty_envelope`, `self_loop`, `already_tombstoned`,
    /// `restore_untombstoned`, `proposal_not_pending`, `node_position_locked`).
    #[error("invalid operation: {0}")]
    InvalidOperation(String),
}
