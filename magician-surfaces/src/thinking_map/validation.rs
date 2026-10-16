//! Live Thinking Map — authority + structural validation (Phase 1b).
//!
//! "The model proposes bounded operations; deterministic code owns state."
//! This module is the safety-critical gate: every operation is checked for
//! **authority** (may this actor emit this op? is its claimed origin honest?)
//! and **structure** (does the target exist? are invariants preserved?) BEFORE
//! it may mutate the candidate map inside the reducer.
//!
//! All functions here are pure and deterministic. Errors carry ONLY ids, enum
//! names, and fixed reason codes — never raw model/user text or `model_trace`
//! contents. This is the belt-and-suspenders boundary against prompt injection:
//! an untrusted `imported` source cannot command privileged operations, and a
//! model cannot claim its inference is owner-authored content.

use crate::thinking_map::errors::{ThinkingMapError, ThinkingMapResult};
use crate::thinking_map::models::{AssertionOrigin, EpistemicState, ThinkingMap};
use crate::thinking_map::operations::{MapOperation, OperationActor};

// ── Controlled string helpers (never leak free text into errors) ─────────────

/// Stable, controlled tag for an actor (the serde `actor` discriminant).
pub fn actor_tag(actor: &OperationActor) -> &'static str {
    match actor {
        OperationActor::Owner { .. } => "owner",
        OperationActor::Participant { .. } => "participant",
        OperationActor::Model { .. } => "model",
        OperationActor::TrustedSystem { .. } => "trusted_system",
        OperationActor::Imported { .. } => "imported",
    }
}

/// Stable, controlled tag for an operation (the serde `op` discriminant).
pub fn op_tag(op: &MapOperation) -> &'static str {
    match op {
        MapOperation::AddNode { .. } => "add_node",
        MapOperation::UpdateNode { .. } => "update_node",
        MapOperation::SetNodeKind { .. } => "set_node_kind",
        MapOperation::SetEpistemicState { .. } => "set_epistemic_state",
        MapOperation::TombstoneNode { .. } => "tombstone_node",
        MapOperation::RestoreNode { .. } => "restore_node",
        MapOperation::Connect { .. } => "connect",
        MapOperation::Disconnect { .. } => "disconnect",
        MapOperation::MoveToParent { .. } => "move_to_parent",
        MapOperation::MoveNode { .. } => "move_node",
        MapOperation::SetPositionLock { .. } => "set_position_lock",
        MapOperation::CreateClarification { .. } => "create_clarification",
        MapOperation::ResolveClarification { .. } => "resolve_clarification",
        MapOperation::ProposeRestructure { .. } => "propose_restructure",
        MapOperation::ConfirmRestructure { .. } => "confirm_restructure",
        MapOperation::RejectRestructure { .. } => "reject_restructure",
        MapOperation::SetSharedView { .. } => "set_shared_view",
        MapOperation::LinkPromotedObject { .. } => "link_promoted_object",
        MapOperation::UnlinkPromotedObject { .. } => "unlink_promoted_object",
        MapOperation::RenameSpeaker { .. } => "rename_speaker",
        MapOperation::SetTitle { .. } => "set_title",
        MapOperation::SetLifecycle { .. } => "set_lifecycle",
    }
}

/// Controlled tag for an assertion origin.
pub fn origin_tag(origin: &AssertionOrigin) -> &'static str {
    match origin {
        AssertionOrigin::OwnerSpoken => "owner_spoken",
        AssertionOrigin::ParticipantSpoken => "participant_spoken",
        AssertionOrigin::OwnerEdited => "owner_edited",
        AssertionOrigin::ImportedSource => "imported_source",
        AssertionOrigin::ModelInferred => "model_inferred",
        AssertionOrigin::SystemDerived => "system_derived",
    }
}

// ── Actor classification (columns of the eligibility matrix) ─────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorClass {
    Owner,
    TrustedSystem,
    Model,
    Participant,
    Imported,
}

impl ActorClass {
    fn of(actor: &OperationActor) -> Self {
        match actor {
            OperationActor::Owner { .. } => ActorClass::Owner,
            OperationActor::TrustedSystem { .. } => ActorClass::TrustedSystem,
            OperationActor::Model { .. } => ActorClass::Model,
            OperationActor::Participant { .. } => ActorClass::Participant,
            OperationActor::Imported { .. } => ActorClass::Imported,
        }
    }
}

fn authority_violation(
    actor: &OperationActor,
    op: &MapOperation,
    reason: &'static str,
) -> ThinkingMapError {
    ThinkingMapError::AuthorityViolation {
        actor: actor_tag(actor).to_string(),
        op: op_tag(op).to_string(),
        reason: reason.to_string(),
    }
}

/// Layer (a) + (b): actor eligibility matrix, the model-subset defensive check,
/// and origin consistency for content-creating operations.
///
/// Returns `Ok(())` if the actor is permitted to emit `op` with the claimed
/// origin, else an `AuthorityViolation` / `OriginMismatch` error carrying only
/// controlled strings.
pub fn check_authority(actor: &OperationActor, op: &MapOperation) -> ThinkingMapResult<()> {
    let class = ActorClass::of(actor);

    // Belt-and-suspenders: the model may only ever emit its declared subset.
    // The matrix below already implies this, but we assert it defensively so a
    // future matrix edit cannot silently widen the model's reach.
    if class == ActorClass::Model && !op.is_model_accessible() {
        return Err(authority_violation(actor, op, "model_subset_violation"));
    }

    // (a) Eligibility matrix. Each arm lists the permitted actor classes.
    let permitted: &[ActorClass] = match op {
        MapOperation::AddNode { .. } | MapOperation::UpdateNode { .. } => &[
            ActorClass::Owner,
            ActorClass::TrustedSystem,
            ActorClass::Model,
            ActorClass::Participant,
            ActorClass::Imported,
        ],
        MapOperation::SetNodeKind { .. }
        | MapOperation::TombstoneNode { .. }
        | MapOperation::RestoreNode { .. }
        | MapOperation::Disconnect { .. }
        | MapOperation::MoveToParent { .. }
        | MapOperation::ResolveClarification { .. }
        | MapOperation::ProposeRestructure { .. } => {
            &[ActorClass::Owner, ActorClass::TrustedSystem, ActorClass::Model]
        }
        // set_epistemic_state is split on the *target state*: `confirmed` is
        // reserved for the owner; every other state is owner/system/model.
        MapOperation::SetEpistemicState { state, .. } => {
            if matches!(state, EpistemicState::Confirmed) {
                if class != ActorClass::Owner {
                    return Err(authority_violation(actor, op, "confirm_reserved_for_owner"));
                }
                &[ActorClass::Owner]
            } else {
                &[ActorClass::Owner, ActorClass::TrustedSystem, ActorClass::Model]
            }
        }
        MapOperation::Connect { .. } => &[
            ActorClass::Owner,
            ActorClass::TrustedSystem,
            ActorClass::Model,
            ActorClass::Participant,
            ActorClass::Imported,
        ],
        MapOperation::CreateClarification { .. } => &[
            ActorClass::Owner,
            ActorClass::TrustedSystem,
            ActorClass::Model,
            ActorClass::Participant,
        ],
        MapOperation::SetSharedView { .. }
        | MapOperation::LinkPromotedObject { .. }
        | MapOperation::UnlinkPromotedObject { .. } => {
            &[ActorClass::Owner, ActorClass::TrustedSystem]
        }
        MapOperation::MoveNode { .. }
        | MapOperation::SetPositionLock { .. }
        | MapOperation::ConfirmRestructure { .. }
        | MapOperation::RejectRestructure { .. }
        | MapOperation::RenameSpeaker { .. }
        // Owner metadata commands: a model may never rename or archive a map.
        | MapOperation::SetTitle { .. }
        | MapOperation::SetLifecycle { .. } => &[ActorClass::Owner],
    };

    if !permitted.contains(&class) {
        return Err(authority_violation(actor, op, "actor_not_permitted"));
    }

    // (b) Origin consistency: for content-creating ops the payload's claimed
    // origin must honestly reflect the acting actor.
    match op {
        MapOperation::AddNode { node } => {
            check_origin_consistency(actor, op, &node.assertion_origin)?;
        },
        MapOperation::Connect { edge } => {
            check_origin_consistency(actor, op, &edge.assertion_origin)?;
        },
        _ => {},
    }

    Ok(())
}

/// The claimed `assertion_origin` on an added node/edge must match the actor.
fn check_origin_consistency(
    actor: &OperationActor,
    _op: &MapOperation,
    claimed: &AssertionOrigin,
) -> ThinkingMapResult<()> {
    let ok = match actor {
        OperationActor::Owner { .. } => matches!(
            claimed,
            AssertionOrigin::OwnerSpoken | AssertionOrigin::OwnerEdited
        ),
        OperationActor::Model { .. } => matches!(claimed, AssertionOrigin::ModelInferred),
        OperationActor::Participant { .. } => {
            matches!(claimed, AssertionOrigin::ParticipantSpoken)
        },
        OperationActor::Imported { .. } => matches!(claimed, AssertionOrigin::ImportedSource),
        OperationActor::TrustedSystem { .. } => {
            matches!(claimed, AssertionOrigin::SystemDerived)
        },
    };
    if ok {
        Ok(())
    } else {
        let expected = expected_origin_tag(actor);
        Err(ThinkingMapError::OriginMismatch {
            expected: expected.to_string(),
            actual: origin_tag(claimed).to_string(),
        })
    }
}

/// The canonical origin tag an actor is expected to claim. Owner may claim
/// `owner_spoken` OR `owner_edited`; we report `owner_spoken` as the canonical
/// representative for error legibility (both are accepted at check time).
fn expected_origin_tag(actor: &OperationActor) -> &'static str {
    match actor {
        OperationActor::Owner { .. } => "owner_spoken",
        OperationActor::Model { .. } => "model_inferred",
        OperationActor::Participant { .. } => "participant_spoken",
        OperationActor::Imported { .. } => "imported_source",
        OperationActor::TrustedSystem { .. } => "system_derived",
    }
}

// ── Structural helpers used by the reducer ───────────────────────────────────

/// Confidence must be within `0.0..=1.0`.
pub fn check_confidence(confidence: f32) -> ThinkingMapResult<()> {
    if (0.0..=1.0).contains(&confidence) {
        Ok(())
    } else {
        Err(ThinkingMapError::ConfidenceOutOfRange(confidence))
    }
}

/// A node exists (present in the map, tombstoned or not).
pub fn require_node(map: &ThinkingMap, node_id: &str) -> ThinkingMapResult<()> {
    if map.nodes.contains_key(node_id) {
        Ok(())
    } else {
        Err(ThinkingMapError::UnknownNode(node_id.to_string()))
    }
}

/// A node exists AND is not tombstoned (used for edge endpoints).
pub fn require_live_node(map: &ThinkingMap, node_id: &str) -> ThinkingMapResult<()> {
    match map.nodes.get(node_id) {
        Some(n) if !n.tombstoned => Ok(()),
        _ => Err(ThinkingMapError::UnknownNode(node_id.to_string())),
    }
}
