//! Live Thinking Map — deterministic reducer + semantic hash (Phase 1b).
//!
//! "The model proposes bounded operations; deterministic code owns state."
//!
//! [`apply_envelope`] is a **pure, deterministic** function: given a map, an
//! envelope, and a caller-injected RFC3339 timestamp, it validates the ENTIRE
//! envelope against a cloned candidate map and only commits if every operation
//! is authorized and structurally valid. It NEVER calls `Utc::now()`, does no
//! IO, and is not async. Partial application is impossible — either the whole
//! envelope applies (yielding a new map at `revision + 1`) or the caller's map
//! is untouched and an error is returned.
//!
//! Idempotency: an envelope already seen (by `envelope_id` OR `idempotency_key`)
//! replays as [`ApplyOutcome::IdempotentReplay`] with the revision it produced,
//! without mutating anything.
//!
//! [`semantic_hash`] produces a stable, order-independent, cross-platform hash
//! of the *durable board content* (revision, lifecycle, nodes, edges,
//! clarifications, proposals) — excluding ephemeral `view_state` and the
//! bookkeeping `applied_envelopes` ledger.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::thinking_map::errors::{ThinkingMapError, ThinkingMapResult};
use crate::thinking_map::models::{
    AppliedEnvelopeRecord, Clarification, EdgeKind, RestructureProposal, ThinkingEdge, ThinkingMap,
    ThinkingNode, APPLIED_ENVELOPE_LEDGER_CAP,
};
use crate::thinking_map::operations::{MapOperation, MapOperationEnvelope, OperationActor};
use crate::thinking_map::validation::{
    check_authority, check_confidence, require_live_node, require_node,
};

/// Outcome of applying an envelope.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    /// The envelope applied cleanly, producing a NEW map.
    Applied {
        map: ThinkingMap,
        resulting_revision: u64,
        semantic_hash: String,
    },
    /// The envelope had already been applied (matched by id or idempotency key);
    /// nothing changed. Carries the revision the original application produced.
    IdempotentReplay { resulting_revision: u64 },
}

/// Apply an operation envelope to `map`, returning a new map (or a replay
/// signal). Pure + deterministic: `applied_at` is the injected RFC3339 clock.
///
/// The caller's `map` is never mutated — all work happens on a clone.
pub fn apply_envelope(
    map: &ThinkingMap,
    envelope: &MapOperationEnvelope,
    applied_at: &str,
) -> ThinkingMapResult<ApplyOutcome> {
    // 1. Empty envelopes are meaningless — fail closed.
    if envelope.operations.is_empty() {
        return Err(ThinkingMapError::InvalidOperation(
            "empty_envelope".to_string(),
        ));
    }

    // 2. Idempotency: replay if we have already applied this envelope (by id OR
    //    idempotency key). No mutation, no error.
    if let Some(rec) = map.applied_envelopes.iter().find(|r| {
        r.envelope_id == envelope.envelope_id || r.idempotency_key == envelope.idempotency_key
    }) {
        return Ok(ApplyOutcome::IdempotentReplay {
            resulting_revision: rec.resulting_revision,
        });
    }

    // 3. Optimistic-concurrency check.
    if envelope.base_revision != map.revision {
        return Err(ThinkingMapError::RevisionConflict {
            expected: map.revision,
            actual: envelope.base_revision,
        });
    }

    // 4. Validate + apply every op against a candidate clone. First failure
    //    aborts and discards the candidate (the input map is never touched).
    let mut candidate = map.clone();
    for op in &envelope.operations {
        check_authority(&envelope.actor, op)?;
        apply_op(&mut candidate, &envelope.actor, op, applied_at)?;
    }

    // 5. Commit: bump revision, stamp, record idempotency, hash.
    let resulting_revision = candidate.revision + 1;
    candidate.revision = resulting_revision;
    candidate.updated_at = applied_at.to_string();
    push_ledger(
        &mut candidate.applied_envelopes,
        AppliedEnvelopeRecord {
            envelope_id: envelope.envelope_id.clone(),
            idempotency_key: envelope.idempotency_key.clone(),
            resulting_revision,
        },
    );
    let semantic_hash = semantic_hash(&candidate);

    Ok(ApplyOutcome::Applied {
        map: candidate,
        resulting_revision,
        semantic_hash,
    })
}

/// Append an idempotency record, evicting the oldest if over cap.
fn push_ledger(ledger: &mut VecDeque<AppliedEnvelopeRecord>, rec: AppliedEnvelopeRecord) {
    ledger.push_back(rec);
    while ledger.len() > APPLIED_ENVELOPE_LEDGER_CAP {
        ledger.pop_front();
    }
}

// ── Per-operation application (authority already checked by caller) ──────────

/// Apply a single, already-authorized operation to the candidate map, running
/// its structural validation inline. `actor` is threaded through because
/// `confirm_restructure` re-applies inner ops with owner authority.
fn apply_op(
    map: &mut ThinkingMap,
    actor: &OperationActor,
    op: &MapOperation,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    match op {
        MapOperation::AddNode { node } => add_node(map, node, applied_at),
        MapOperation::UpdateNode {
            node_id,
            label,
            detail_markdown,
            confidence,
        } => update_node(map, node_id, label, detail_markdown, confidence, applied_at),
        MapOperation::SetNodeKind { node_id, kind } => {
            require_node(map, node_id)?;
            let n = map.nodes.get_mut(node_id).expect("checked");
            n.kind = *kind;
            n.updated_at = applied_at.to_string();
            Ok(())
        },
        MapOperation::SetEpistemicState { node_id, state } => {
            require_node(map, node_id)?;
            let n = map.nodes.get_mut(node_id).expect("checked");
            n.epistemic_state = *state;
            n.updated_at = applied_at.to_string();
            Ok(())
        },
        MapOperation::TombstoneNode { node_id } => tombstone_node(map, node_id, applied_at),
        MapOperation::RestoreNode { node_id } => restore_node(map, node_id, applied_at),
        MapOperation::Connect { edge } => connect(map, edge, applied_at),
        MapOperation::Disconnect { edge_id } => disconnect(map, edge_id, applied_at),
        MapOperation::MoveToParent { node_id, parent_id } => {
            move_to_parent(map, node_id, parent_id, applied_at)
        },
        MapOperation::MoveNode { node_id, position } => {
            move_node(map, actor, node_id, position, applied_at)
        },
        MapOperation::SetPositionLock { node_id, locked } => {
            require_node(map, node_id)?;
            let n = map.nodes.get_mut(node_id).expect("checked");
            n.position_locked = *locked;
            n.updated_at = applied_at.to_string();
            Ok(())
        },
        MapOperation::CreateClarification { clarification } => {
            create_clarification(map, clarification)
        },
        MapOperation::ResolveClarification {
            clarification_id,
            state,
            answer,
        } => resolve_clarification(map, clarification_id, state, answer, applied_at),
        MapOperation::ProposeRestructure { proposal } => propose_restructure(map, actor, proposal),
        MapOperation::ConfirmRestructure { proposal_id } => {
            confirm_restructure(map, proposal_id, applied_at)
        },
        MapOperation::RejectRestructure { proposal_id } => {
            reject_restructure(map, proposal_id, applied_at)
        },
        MapOperation::SetSharedView { view_state } => {
            map.view_state = view_state.clone();
            Ok(())
        },
        MapOperation::LinkPromotedObject { node_id, promoted } => {
            link_promoted_object(map, node_id, promoted, applied_at)
        },
        MapOperation::UnlinkPromotedObject {
            node_id,
            destination_kind,
            object_id,
        } => unlink_promoted_object(map, node_id, *destination_kind, object_id, applied_at),
        MapOperation::RenameSpeaker {
            old_speaker_id,
            new_display_name,
        } => rename_speaker(map, old_speaker_id, new_display_name, applied_at),
        // Owner-only metadata commands (authority already checked). Event-sourced
        // + replay-consistent: `revision` and `lifecycle` are in the semantic
        // hash, so the fold reproduces each transition on replay. `title` is NOT
        // independently hashed, and `replay.rs` reconstructs its revision-0 base
        // from the manifest's CURRENT title — so a historical
        // `replay_to_sequence(N)` reflects the current title, which (title being
        // absent from the semantic hash) never causes replay divergence.
        MapOperation::SetTitle { title } => {
            if title.trim().is_empty() {
                return Err(ThinkingMapError::InvalidOperation(
                    "empty_title".to_string(),
                ));
            }
            map.title = title.clone();
            map.updated_at = applied_at.to_string();
            Ok(())
        },
        MapOperation::SetLifecycle { lifecycle } => {
            map.lifecycle = *lifecycle;
            map.updated_at = applied_at.to_string();
            Ok(())
        },
    }
}

fn add_node(map: &mut ThinkingMap, node: &ThinkingNode, applied_at: &str) -> ThinkingMapResult<()> {
    // Reject if the id is present in any form (tombstoned nodes must be revived
    // via restore_node, not re-added).
    if map.nodes.contains_key(&node.node_id) {
        return Err(ThinkingMapError::DuplicateNode(node.node_id.clone()));
    }
    check_confidence(node.confidence)?;
    let mut node = node.clone();
    node.updated_at = applied_at.to_string();
    map.nodes.insert(node.node_id.clone(), node);
    Ok(())
}

fn update_node(
    map: &mut ThinkingMap,
    node_id: &str,
    label: &Option<String>,
    detail_markdown: &Option<Option<String>>,
    confidence: &Option<f32>,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    require_node(map, node_id)?;
    if let Some(c) = confidence {
        check_confidence(*c)?;
    }
    let n = map.nodes.get_mut(node_id).expect("checked");
    if let Some(l) = label {
        n.label = l.clone();
    }
    if let Some(d) = detail_markdown {
        n.detail_markdown = d.clone();
    }
    if let Some(c) = confidence {
        n.confidence = *c;
    }
    n.updated_at = applied_at.to_string();
    Ok(())
}

fn tombstone_node(map: &mut ThinkingMap, node_id: &str, applied_at: &str) -> ThinkingMapResult<()> {
    match map.nodes.get(node_id) {
        None => return Err(ThinkingMapError::UnknownNode(node_id.to_string())),
        Some(n) if n.tombstoned => {
            return Err(ThinkingMapError::InvalidOperation(
                "already_tombstoned".to_string(),
            ))
        },
        Some(_) => {},
    }
    {
        let n = map.nodes.get_mut(node_id).expect("checked");
        n.tombstoned = true;
        n.updated_at = applied_at.to_string();
    }
    // Cascade: tombstone every non-tombstoned edge incident to this node.
    // History is preserved — edges are flagged, never removed.
    for edge in map.edges.values_mut() {
        if !edge.tombstoned && (edge.from_node == node_id || edge.to_node == node_id) {
            edge.tombstoned = true;
            edge.updated_at = applied_at.to_string();
        }
    }
    Ok(())
}

fn restore_node(map: &mut ThinkingMap, node_id: &str, applied_at: &str) -> ThinkingMapResult<()> {
    match map.nodes.get(node_id) {
        None => return Err(ThinkingMapError::UnknownNode(node_id.to_string())),
        Some(n) if !n.tombstoned => {
            return Err(ThinkingMapError::InvalidOperation(
                "restore_untombstoned".to_string(),
            ))
        },
        Some(_) => {},
    }
    // Design choice: restore un-tombstones ONLY the node, not its edges. A prior
    // tombstone cascade is not automatically reversed — the owner reconnects
    // explicitly. This keeps restore a single, predictable, non-magical action.
    let n = map.nodes.get_mut(node_id).expect("checked");
    n.tombstoned = false;
    n.updated_at = applied_at.to_string();
    Ok(())
}

fn connect(map: &mut ThinkingMap, edge: &ThinkingEdge, applied_at: &str) -> ThinkingMapResult<()> {
    if map.edges.contains_key(&edge.edge_id) {
        return Err(ThinkingMapError::DuplicateEdge(edge.edge_id.clone()));
    }
    if edge.from_node == edge.to_node {
        return Err(ThinkingMapError::InvalidOperation("self_loop".to_string()));
    }
    // Both endpoints must exist and be live.
    require_live_node(map, &edge.from_node)?;
    require_live_node(map, &edge.to_node)?;

    // Acyclicity only for hierarchical/dependency kinds. Check within the
    // subgraph of edges of THIS kind (non-tombstoned), treating the new edge as
    // present. A cycle exists iff `to_node` can already reach `from_node`.
    if is_cycle_forbidden_kind(&edge.kind)
        && reaches(map, &edge.kind, &edge.to_node, &edge.from_node)
    {
        return Err(ThinkingMapError::CycleDetected {
            edge_kind: edge_kind_tag(&edge.kind).to_string(),
        });
    }

    let mut edge = edge.clone();
    edge.updated_at = applied_at.to_string();
    map.edges.insert(edge.edge_id.clone(), edge);
    Ok(())
}

fn disconnect(map: &mut ThinkingMap, edge_id: &str, applied_at: &str) -> ThinkingMapResult<()> {
    let edge = map
        .edges
        .get_mut(edge_id)
        .ok_or_else(|| ThinkingMapError::UnknownEdge(edge_id.to_string()))?;
    // Soft-remove: flag tombstoned, keep history.
    edge.tombstoned = true;
    edge.updated_at = applied_at.to_string();
    Ok(())
}

fn move_to_parent(
    map: &mut ThinkingMap,
    node_id: &str,
    parent_id: &Option<String>,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    require_node(map, node_id)?;
    if let Some(pid) = parent_id {
        // A node cannot be its own parent, nor become its own ancestor.
        if pid == node_id {
            return Err(ThinkingMapError::ParentCycle);
        }
        require_node(map, pid)?;
        // Walk the prospective parent's ancestor chain; if we ever hit node_id,
        // setting this parent would close a cycle.
        let mut cursor = Some(pid.clone());
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(cur) = cursor {
            if cur == node_id {
                return Err(ThinkingMapError::ParentCycle);
            }
            if !seen.insert(cur.clone()) {
                // Pre-existing cycle in stored parents; stop to avoid looping.
                break;
            }
            cursor = map.nodes.get(&cur).and_then(|n| n.parent_id.clone());
        }
    }
    let n = map.nodes.get_mut(node_id).expect("checked");
    n.parent_id = parent_id.clone();
    n.updated_at = applied_at.to_string();
    Ok(())
}

fn move_node(
    map: &mut ThinkingMap,
    actor: &OperationActor,
    node_id: &str,
    position: &crate::thinking_map::models::Position,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    require_node(map, node_id)?;
    let locked = map.nodes.get(node_id).expect("checked").position_locked;
    // Only the owner may move a position-locked node. (The matrix already makes
    // move_node owner-only, so this is defense-in-depth for a future widening.)
    if locked && !matches!(actor, OperationActor::Owner { .. }) {
        return Err(ThinkingMapError::AuthorityViolation {
            actor: crate::thinking_map::validation::actor_tag(actor).to_string(),
            op: "move_node".to_string(),
            reason: "node_position_locked".to_string(),
        });
    }
    let n = map.nodes.get_mut(node_id).expect("checked");
    n.position = Some(position.clone());
    n.updated_at = applied_at.to_string();
    Ok(())
}

fn create_clarification(
    map: &mut ThinkingMap,
    clarification: &Clarification,
) -> ThinkingMapResult<()> {
    if map
        .clarifications
        .contains_key(&clarification.clarification_id)
    {
        return Err(ThinkingMapError::DuplicateClarification(
            clarification.clarification_id.clone(),
        ));
    }
    require_node(map, &clarification.node_id)?;
    map.clarifications.insert(
        clarification.clarification_id.clone(),
        clarification.clone(),
    );
    Ok(())
}

fn resolve_clarification(
    map: &mut ThinkingMap,
    clarification_id: &str,
    state: &crate::thinking_map::models::ClarificationState,
    answer: &Option<String>,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    let c = map
        .clarifications
        .get_mut(clarification_id)
        .ok_or_else(|| ThinkingMapError::UnknownClarification(clarification_id.to_string()))?;
    c.state = *state;
    c.answer = answer.clone();
    c.resolved_at = Some(applied_at.to_string());
    Ok(())
}

/// Restructure proposals may not contain meta-restructure operations. Nesting
/// `propose`/`confirm`/`reject` inside a proposal would allow recursion and a
/// path to escalate authority through chained confirmations.
fn reject_meta_restructure(op: &MapOperation) -> ThinkingMapResult<()> {
    match op {
        MapOperation::ProposeRestructure { .. }
        | MapOperation::ConfirmRestructure { .. }
        | MapOperation::RejectRestructure { .. } => Err(ThinkingMapError::InvalidOperation(
            "nested_restructure_forbidden".to_string(),
        )),
        _ => Ok(()),
    }
}

fn propose_restructure(
    map: &mut ThinkingMap,
    actor: &OperationActor,
    proposal: &RestructureProposal,
) -> ThinkingMapResult<()> {
    if map.proposals.contains_key(&proposal.proposal_id) {
        return Err(ThinkingMapError::DuplicateProposal(
            proposal.proposal_id.clone(),
        ));
    }
    // A restructure may not stage operations its author could not emit directly,
    // nor may it nest meta-restructure ops. Validating the inner ops against the
    // PROPOSING actor here (and again at confirm) is what prevents an untrusted
    // author from laundering an owner-only op through a one-click owner confirm.
    for inner in &proposal.operations {
        reject_meta_restructure(inner)?;
        check_authority(actor, inner)?;
    }
    // Stamp the authoritative proposer; any client-supplied `proposed_by` is
    // ignored (a proposer cannot claim to be someone with more authority).
    let mut proposal = proposal.clone();
    proposal.proposed_by = actor.clone();
    map.proposals.insert(proposal.proposal_id.clone(), proposal);
    Ok(())
}

fn confirm_restructure(
    map: &mut ThinkingMap,
    proposal_id: &str,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    let proposal = map
        .proposals
        .get(proposal_id)
        .ok_or_else(|| ThinkingMapError::UnknownProposal(proposal_id.to_string()))?;
    if !matches!(
        proposal.state,
        crate::thinking_map::models::ProposalState::Proposed
    ) {
        return Err(ThinkingMapError::InvalidOperation(
            "proposal_not_pending".to_string(),
        ));
    }
    // Re-apply the proposal's inner ops under the ORIGINAL proposer's authority
    // (never blanket owner), so a confirmed proposal can never exceed what its
    // author was allowed to do. Owner confirmation authorizes *applying* the
    // proposal; it does not elevate the proposer's ops. This also preserves
    // provenance — a model-proposed `model_inferred` node stays model-authored
    // instead of being rejected as dishonest owner content. Any inner failure
    // propagates and fails the whole envelope (atomicity). Defense-in-depth: the
    // meta-restructure ban and per-op authority are re-checked here.
    let inner_ops = proposal.operations.clone();
    let proposer = proposal.proposed_by.clone();
    for inner in &inner_ops {
        reject_meta_restructure(inner)?;
        check_authority(&proposer, inner)?;
        apply_op(map, &proposer, inner, applied_at)?;
    }
    let p = map.proposals.get_mut(proposal_id).expect("checked");
    p.state = crate::thinking_map::models::ProposalState::Confirmed;
    p.resolved_at = Some(applied_at.to_string());
    Ok(())
}

fn reject_restructure(
    map: &mut ThinkingMap,
    proposal_id: &str,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    let p = map
        .proposals
        .get_mut(proposal_id)
        .ok_or_else(|| ThinkingMapError::UnknownProposal(proposal_id.to_string()))?;
    if !matches!(
        p.state,
        crate::thinking_map::models::ProposalState::Proposed
    ) {
        return Err(ThinkingMapError::InvalidOperation(
            "proposal_not_pending".to_string(),
        ));
    }
    p.state = crate::thinking_map::models::ProposalState::Rejected;
    p.resolved_at = Some(applied_at.to_string());
    Ok(())
}

fn link_promoted_object(
    map: &mut ThinkingMap,
    node_id: &str,
    promoted: &crate::thinking_map::models::PromotedRef,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    require_node(map, node_id)?;
    // Uniqueness: (destination_kind, object_id) may be linked to at most ONE
    // node anywhere in the map.
    for (nid, n) in map.nodes.iter() {
        for existing in &n.promoted_refs {
            if existing.destination_kind == promoted.destination_kind
                && existing.object_id == promoted.object_id
            {
                if nid == node_id {
                    // Exact same (node, kind, object) already linked → idempotent
                    // no-op; do not duplicate.
                    return Ok(());
                }
                return Err(ThinkingMapError::PromotionLinkConflict {
                    destination_kind: promotion_kind_tag(&promoted.destination_kind).to_string(),
                    object_id: promoted.object_id.clone(),
                });
            }
        }
    }
    let n = map.nodes.get_mut(node_id).expect("checked");
    let mut promoted = promoted.clone();
    promoted.linked_at = applied_at.to_string();
    n.promoted_refs.push(promoted);
    n.updated_at = applied_at.to_string();
    Ok(())
}

fn unlink_promoted_object(
    map: &mut ThinkingMap,
    node_id: &str,
    destination_kind: crate::thinking_map::models::PromotionKind,
    object_id: &str,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    require_node(map, node_id)?;
    let n = map.nodes.get_mut(node_id).expect("checked");
    let before = n.promoted_refs.len();
    n.promoted_refs
        .retain(|r| !(r.destination_kind == destination_kind && r.object_id == object_id));
    if n.promoted_refs.len() != before {
        n.updated_at = applied_at.to_string();
    }
    // Absent ref → no-op (still Ok).
    Ok(())
}

fn rename_speaker(
    map: &mut ThinkingMap,
    old_speaker_id: &str,
    new_display_name: &str,
    applied_at: &str,
) -> ThinkingMapResult<()> {
    for n in map.nodes.values_mut() {
        if let Some(spk) = n.speaker.as_mut() {
            if spk.speaker_id == old_speaker_id {
                spk.display_name = Some(new_display_name.to_string());
                n.updated_at = applied_at.to_string();
            }
        }
    }
    Ok(())
}

// ── Cycle detection helpers ──────────────────────────────────────────────────

fn is_cycle_forbidden_kind(kind: &EdgeKind) -> bool {
    matches!(kind, EdgeKind::GroupedUnder | EdgeKind::DependsOn)
}

fn edge_kind_tag(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::RelatedTo => "related_to",
        EdgeKind::Supports => "supports",
        EdgeKind::Contradicts => "contradicts",
        EdgeKind::Answers => "answers",
        EdgeKind::DependsOn => "depends_on",
        EdgeKind::LeadsTo => "leads_to",
        EdgeKind::AlternativeTo => "alternative_to",
        EdgeKind::Measures => "measures",
        EdgeKind::GroupedUnder => "grouped_under",
    }
}

fn promotion_kind_tag(kind: &crate::thinking_map::models::PromotionKind) -> &'static str {
    use crate::thinking_map::models::PromotionKind;
    match kind {
        PromotionKind::Task => "task",
        PromotionKind::Today => "today",
        PromotionKind::Memory => "memory",
    }
}

/// Does `start` reach `target` following non-tombstoned edges of exactly
/// `kind`? Deterministic DFS over the BTreeMap-ordered edge set.
fn reaches(map: &ThinkingMap, kind: &EdgeKind, start: &str, target: &str) -> bool {
    if start == target {
        return true;
    }
    let mut stack = vec![start.to_string()];
    let mut visited: BTreeSet<String> = BTreeSet::new();
    while let Some(cur) = stack.pop() {
        if !visited.insert(cur.clone()) {
            continue;
        }
        for edge in map.edges.values() {
            if edge.tombstoned || edge.kind != *kind {
                continue;
            }
            if edge.from_node == cur {
                if edge.to_node == target {
                    return true;
                }
                stack.push(edge.to_node.clone());
            }
        }
    }
    false
}

// ── Semantic hash ────────────────────────────────────────────────────────────

/// A normalized, canonical projection of the durable board content used for
/// hashing. `BTreeMap` gives a stable key order, so serde_json emits bytes that
/// are identical across process runs and platforms. `view_state` and
/// `applied_envelopes` are intentionally omitted.
#[derive(Serialize)]
struct SemanticProjection<'a> {
    schema_version: u32,
    revision: u64,
    lifecycle: &'a crate::thinking_map::models::MapLifecycle,
    nodes: &'a BTreeMap<String, ThinkingNode>,
    edges: &'a BTreeMap<String, ThinkingEdge>,
    clarifications: &'a BTreeMap<String, Clarification>,
    proposals: &'a BTreeMap<String, RestructureProposal>,
}

/// Deterministic, order-independent, cross-platform hash of the durable board.
///
/// Excludes `view_state` (ephemeral shared-cursor state) and `applied_envelopes`
/// (idempotency bookkeeping). Uses SHA-256 over the canonical serde_json bytes
/// of a normalized projection; returns a lowercase hex digest.
pub fn semantic_hash(map: &ThinkingMap) -> String {
    let projection = SemanticProjection {
        schema_version: map.schema_version,
        revision: map.revision,
        lifecycle: &map.lifecycle,
        nodes: &map.nodes,
        edges: &map.edges,
        clarifications: &map.clarifications,
        proposals: &map.proposals,
    };
    // serde_json over BTreeMaps is deterministic. to_vec cannot fail for this
    // fully-serializable projection; fall back to a fixed sentinel on the
    // impossible error so we never panic in the reducer.
    let bytes =
        serde_json::to_vec(&projection).unwrap_or_else(|_| b"thinking_map_hash_error".to_vec());
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    hex_lower(&digest)
}

/// Lowercase hex encoding without pulling an extra crate.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking_map::models::{
        AssertionOrigin, Clarification, ClarificationState, EdgeKind, EpistemicState, MapLifecycle,
        NodeKind, Position, PromotedRef, PromotionKind, ProposalState, RestructureProposal,
        SharedViewState, SpeakerRef, ThinkingEdge, ThinkingMap, ThinkingMapSource, ThinkingNode,
        ViewLens,
    };
    use crate::thinking_map::operations::{MapOperation, MapOperationEnvelope, OperationActor};

    const TS: &str = "2026-07-19T00:00:00Z";
    const TS2: &str = "2026-07-19T01:00:00Z";

    fn empty_map() -> ThinkingMap {
        ThinkingMap::new(
            "map-1".to_string(),
            "anonymous",
            "default",
            "Test map",
            ThinkingMapSource::Solo,
            TS,
        )
    }

    fn owner() -> OperationActor {
        OperationActor::Owner {
            principal: "anonymous".to_string(),
        }
    }

    fn model() -> OperationActor {
        OperationActor::Model {
            trace_id: Some("tr-1".to_string()),
        }
    }

    fn participant() -> OperationActor {
        OperationActor::Participant {
            speaker_id: "spk-2".to_string(),
        }
    }

    fn trusted() -> OperationActor {
        OperationActor::TrustedSystem {
            component: "distiller".to_string(),
        }
    }

    fn imported() -> OperationActor {
        OperationActor::Imported {
            source_kind: "email".to_string(),
        }
    }

    fn node(id: &str, origin: AssertionOrigin) -> ThinkingNode {
        ThinkingNode {
            node_id: id.to_string(),
            kind: NodeKind::Idea,
            label: format!("label-{id}"),
            detail_markdown: None,
            epistemic_state: EpistemicState::Provisional,
            assertion_origin: origin,
            confidence: 0.5,
            speaker: None,
            source_refs: vec![],
            parent_id: None,
            position: None,
            position_locked: false,
            promoted_refs: vec![],
            tombstoned: false,
            created_at: TS.to_string(),
            updated_at: TS.to_string(),
        }
    }

    fn edge(
        id: &str,
        from: &str,
        to: &str,
        kind: EdgeKind,
        origin: AssertionOrigin,
    ) -> ThinkingEdge {
        ThinkingEdge {
            edge_id: id.to_string(),
            from_node: from.to_string(),
            to_node: to.to_string(),
            kind,
            assertion_origin: origin,
            tombstoned: false,
            created_at: TS.to_string(),
            updated_at: TS.to_string(),
        }
    }

    fn envelope(
        map: &ThinkingMap,
        actor: OperationActor,
        idem: &str,
        ops: Vec<MapOperation>,
    ) -> MapOperationEnvelope {
        MapOperationEnvelope::new(
            format!("env-{idem}"),
            map.map_id.clone(),
            map.revision,
            actor,
            idem,
            ops,
            TS,
        )
    }

    /// Apply and unwrap the resulting map, asserting `Applied`.
    fn apply_ok(map: &ThinkingMap, env: &MapOperationEnvelope, at: &str) -> ThinkingMap {
        match apply_envelope(map, env, at).expect("apply should succeed") {
            ApplyOutcome::Applied { map, .. } => map,
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    // ── Happy path ───────────────────────────────────────────────────────────

    #[test]
    fn happy_path_add_node() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "i1",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::OwnerSpoken),
            }],
        );
        let outcome = apply_envelope(&map, &env, TS2).unwrap();
        match outcome {
            ApplyOutcome::Applied {
                map: new_map,
                resulting_revision,
                semantic_hash,
            } => {
                assert_eq!(resulting_revision, 1);
                assert_eq!(new_map.revision, 1);
                assert!(new_map.nodes.contains_key("n1"));
                assert_eq!(new_map.updated_at, TS2);
                assert_eq!(new_map.nodes["n1"].updated_at, TS2);
                assert!(!semantic_hash.is_empty());
            },
            other => panic!("expected Applied, got {other:?}"),
        }
        // Input untouched.
        assert_eq!(map.revision, 0);
        assert!(map.nodes.is_empty());
    }

    // ── Atomicity ──────────────────────────────────────────────────────────────

    #[test]
    fn atomicity_second_op_invalid_rolls_back() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "i1",
            vec![
                MapOperation::AddNode {
                    node: node("n1", AssertionOrigin::OwnerSpoken),
                },
                // References a node that does not exist.
                MapOperation::SetEpistemicState {
                    node_id: "does-not-exist".to_string(),
                    state: EpistemicState::Asserted,
                },
            ],
        );
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::UnknownNode("does-not-exist".to_string())
        );
        // Original untouched, and no partial node.
        assert_eq!(map.revision, 0);
        assert!(map.nodes.is_empty());
    }

    // ── Idempotency ────────────────────────────────────────────────────────────

    #[test]
    fn idempotent_replay_same_envelope() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "i1",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::OwnerSpoken),
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        assert_eq!(map1.revision, 1);
        // Re-apply the SAME envelope against the new map.
        let outcome = apply_envelope(&map1, &env, TS2).unwrap();
        assert_eq!(
            outcome,
            ApplyOutcome::IdempotentReplay {
                resulting_revision: 1
            }
        );
    }

    #[test]
    fn idempotent_replay_same_key_different_envelope() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "shared-key",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::OwnerSpoken),
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        // Different envelope_id, SAME idempotency_key.
        let env2 = MapOperationEnvelope::new(
            "env-different".to_string(),
            map1.map_id.clone(),
            map1.revision,
            owner(),
            "shared-key",
            vec![MapOperation::AddNode {
                node: node("n2", AssertionOrigin::OwnerSpoken),
            }],
            TS,
        );
        let outcome = apply_envelope(&map1, &env2, TS2).unwrap();
        assert_eq!(
            outcome,
            ApplyOutcome::IdempotentReplay {
                resulting_revision: 1
            }
        );
        // n2 was NOT added.
        assert!(!map1.nodes.contains_key("n2"));
    }

    // ── Revision conflict ──────────────────────────────────────────────────────

    #[test]
    fn revision_conflict() {
        let map = empty_map();
        let mut env = envelope(
            &map,
            owner(),
            "i1",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::OwnerSpoken),
            }],
        );
        env.base_revision = 5; // != map.revision (0)
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::RevisionConflict {
                expected: 0,
                actual: 5
            }
        );
    }

    #[test]
    fn empty_envelope_rejected() {
        let map = empty_map();
        let env = envelope(&map, owner(), "i1", vec![]);
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::InvalidOperation("empty_envelope".to_string())
        );
    }

    // ── Authority matrix (table-driven) ─────────────────────────────────────────

    fn base_map_with_node() -> ThinkingMap {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "seed",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::OwnerSpoken),
            }],
        );
        apply_ok(&map, &env, TS)
    }

    #[test]
    fn authority_matrix_representative() {
        let map = base_map_with_node();

        // participant confirm_restructure ⇒ violation.
        {
            let env = envelope(
                &map,
                participant(),
                "a1",
                vec![MapOperation::ConfirmRestructure {
                    proposal_id: "p1".to_string(),
                }],
            );
            let err = apply_envelope(&map, &env, TS2).unwrap_err();
            assert!(matches!(err, ThinkingMapError::AuthorityViolation { .. }));
        }

        // model set_epistemic_state → confirmed ⇒ violation.
        {
            let env = envelope(
                &map,
                model(),
                "a2",
                vec![MapOperation::SetEpistemicState {
                    node_id: "n1".to_string(),
                    state: EpistemicState::Confirmed,
                }],
            );
            let err = apply_envelope(&map, &env, TS2).unwrap_err();
            match err {
                ThinkingMapError::AuthorityViolation { reason, .. } => {
                    assert_eq!(reason, "confirm_reserved_for_owner");
                },
                other => panic!("expected AuthorityViolation, got {other:?}"),
            }
        }

        // model move_node ⇒ violation (subset + matrix).
        {
            let env = envelope(
                &map,
                model(),
                "a3",
                vec![MapOperation::MoveNode {
                    node_id: "n1".to_string(),
                    position: Position { x: 1.0, y: 1.0 },
                }],
            );
            let err = apply_envelope(&map, &env, TS2).unwrap_err();
            assert!(matches!(err, ThinkingMapError::AuthorityViolation { .. }));
        }

        // imported create_clarification ⇒ violation.
        {
            let env = envelope(
                &map,
                imported(),
                "a4",
                vec![MapOperation::CreateClarification {
                    clarification: Clarification {
                        clarification_id: "c1".to_string(),
                        node_id: "n1".to_string(),
                        question: "?".to_string(),
                        state: ClarificationState::Open,
                        answer: None,
                        created_at: TS.to_string(),
                        resolved_at: None,
                    },
                }],
            );
            let err = apply_envelope(&map, &env, TS2).unwrap_err();
            assert!(matches!(err, ThinkingMapError::AuthorityViolation { .. }));
        }

        // trusted_system link_promoted_object ⇒ allowed.
        {
            let env = envelope(
                &map,
                trusted(),
                "a5",
                vec![MapOperation::LinkPromotedObject {
                    node_id: "n1".to_string(),
                    promoted: PromotedRef {
                        destination_kind: PromotionKind::Task,
                        object_id: "task-9".to_string(),
                        linked_at: TS.to_string(),
                    },
                }],
            );
            let new_map = apply_ok(&map, &env, TS2);
            assert_eq!(new_map.nodes["n1"].promoted_refs.len(), 1);
        }

        // owner everything (a representative privileged op) ⇒ allowed.
        {
            let env = envelope(
                &map,
                owner(),
                "a6",
                vec![MapOperation::SetEpistemicState {
                    node_id: "n1".to_string(),
                    state: EpistemicState::Confirmed,
                }],
            );
            let new_map = apply_ok(&map, &env, TS2);
            assert_eq!(
                new_map.nodes["n1"].epistemic_state,
                EpistemicState::Confirmed
            );
        }
    }

    // ── Origin consistency ─────────────────────────────────────────────────────

    #[test]
    fn origin_mismatch_model_claims_owner() {
        let map = empty_map();
        let env = envelope(
            &map,
            model(),
            "i1",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::OwnerSpoken),
            }],
        );
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::OriginMismatch {
                expected: "model_inferred".to_string(),
                actual: "owner_spoken".to_string(),
            }
        );
    }

    #[test]
    fn origin_ok_model_infers() {
        let map = empty_map();
        let env = envelope(
            &map,
            model(),
            "i1",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::ModelInferred),
            }],
        );
        let new_map = apply_ok(&map, &env, TS2);
        assert!(new_map.nodes.contains_key("n1"));
    }

    // ── Correction / supersession ──────────────────────────────────────────────

    #[test]
    fn supersede_keeps_history() {
        let map = empty_map();
        let mut prov = node("n1", AssertionOrigin::OwnerSpoken);
        prov.epistemic_state = EpistemicState::Provisional;
        let env = envelope(
            &map,
            owner(),
            "i1",
            vec![MapOperation::AddNode { node: prov }],
        );
        let map1 = apply_ok(&map, &env, TS);

        let env2 = envelope(
            &map1,
            owner(),
            "i2",
            vec![MapOperation::SetEpistemicState {
                node_id: "n1".to_string(),
                state: EpistemicState::Superseded,
            }],
        );
        let map2 = apply_ok(&map1, &env2, TS2);
        assert_eq!(map2.nodes["n1"].epistemic_state, EpistemicState::Superseded);
        // Node still present (history intact) — not deleted.
        assert!(map2.nodes.contains_key("n1"));
        assert_eq!(map2.nodes["n1"].updated_at, TS2);
    }

    // ── Tombstone cascade ──────────────────────────────────────────────────────

    #[test]
    fn tombstone_cascades_incident_edges() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "seed",
            vec![
                MapOperation::AddNode {
                    node: node("n1", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("n2", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::Connect {
                    edge: edge(
                        "e1",
                        "n1",
                        "n2",
                        EdgeKind::RelatedTo,
                        AssertionOrigin::OwnerSpoken,
                    ),
                },
            ],
        );
        let map1 = apply_ok(&map, &env, TS);

        let env2 = envelope(
            &map1,
            owner(),
            "tomb",
            vec![MapOperation::TombstoneNode {
                node_id: "n1".to_string(),
            }],
        );
        let map2 = apply_ok(&map1, &env2, TS2);
        assert!(map2.nodes["n1"].tombstoned);
        // Edge tombstoned but still present (history preserved).
        assert!(map2.edges.contains_key("e1"));
        assert!(map2.edges["e1"].tombstoned);
        assert_eq!(map2.edges["e1"].updated_at, TS2);
        // The other node untouched.
        assert!(!map2.nodes["n2"].tombstoned);
    }

    #[test]
    fn tombstone_already_tombstoned_rejected() {
        let map = base_map_with_node();
        let env = envelope(
            &map,
            owner(),
            "t1",
            vec![MapOperation::TombstoneNode {
                node_id: "n1".to_string(),
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        let env2 = envelope(
            &map1,
            owner(),
            "t2",
            vec![MapOperation::TombstoneNode {
                node_id: "n1".to_string(),
            }],
        );
        let err = apply_envelope(&map1, &env2, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::InvalidOperation("already_tombstoned".to_string())
        );
    }

    #[test]
    fn restore_untombstoned_rejected() {
        let map = base_map_with_node();
        let env = envelope(
            &map,
            owner(),
            "r1",
            vec![MapOperation::RestoreNode {
                node_id: "n1".to_string(),
            }],
        );
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::InvalidOperation("restore_untombstoned".to_string())
        );
    }

    // ── Acyclicity ─────────────────────────────────────────────────────────────

    #[test]
    fn grouped_under_cycle_detected() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "seed",
            vec![
                MapOperation::AddNode {
                    node: node("a", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("b", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("c", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::Connect {
                    edge: edge(
                        "e1",
                        "a",
                        "b",
                        EdgeKind::GroupedUnder,
                        AssertionOrigin::OwnerSpoken,
                    ),
                },
                MapOperation::Connect {
                    edge: edge(
                        "e2",
                        "b",
                        "c",
                        EdgeKind::GroupedUnder,
                        AssertionOrigin::OwnerSpoken,
                    ),
                },
            ],
        );
        let map1 = apply_ok(&map, &env, TS);
        // c → a closes a cycle.
        let env2 = envelope(
            &map1,
            owner(),
            "cyc",
            vec![MapOperation::Connect {
                edge: edge(
                    "e3",
                    "c",
                    "a",
                    EdgeKind::GroupedUnder,
                    AssertionOrigin::OwnerSpoken,
                ),
            }],
        );
        let err = apply_envelope(&map1, &env2, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::CycleDetected {
                edge_kind: "grouped_under".to_string()
            }
        );
    }

    #[test]
    fn related_to_cycle_allowed() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "seed",
            vec![
                MapOperation::AddNode {
                    node: node("a", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("b", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::Connect {
                    edge: edge(
                        "e1",
                        "a",
                        "b",
                        EdgeKind::RelatedTo,
                        AssertionOrigin::OwnerSpoken,
                    ),
                },
                MapOperation::Connect {
                    edge: edge(
                        "e2",
                        "b",
                        "a",
                        EdgeKind::RelatedTo,
                        AssertionOrigin::OwnerSpoken,
                    ),
                },
            ],
        );
        // Cycles are fine for general kinds → applies cleanly.
        let map1 = apply_ok(&map, &env, TS);
        assert_eq!(map1.edges.len(), 2);
    }

    #[test]
    fn self_loop_rejected() {
        let map = base_map_with_node();
        let env = envelope(
            &map,
            owner(),
            "s1",
            vec![MapOperation::Connect {
                edge: edge(
                    "e1",
                    "n1",
                    "n1",
                    EdgeKind::RelatedTo,
                    AssertionOrigin::OwnerSpoken,
                ),
            }],
        );
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::InvalidOperation("self_loop".to_string())
        );
    }

    #[test]
    fn parent_cycle_rejected() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "seed",
            vec![
                MapOperation::AddNode {
                    node: node("a", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("b", AssertionOrigin::OwnerSpoken),
                },
                // a's parent = b
                MapOperation::MoveToParent {
                    node_id: "a".to_string(),
                    parent_id: Some("b".to_string()),
                },
            ],
        );
        let map1 = apply_ok(&map, &env, TS);
        // Now b's parent = a would close a cycle.
        let env2 = envelope(
            &map1,
            owner(),
            "cyc",
            vec![MapOperation::MoveToParent {
                node_id: "b".to_string(),
                parent_id: Some("a".to_string()),
            }],
        );
        let err = apply_envelope(&map1, &env2, TS2).unwrap_err();
        assert_eq!(err, ThinkingMapError::ParentCycle);
    }

    // ── Promotion uniqueness ────────────────────────────────────────────────────

    #[test]
    fn promotion_uniqueness_and_idempotence() {
        let map = empty_map();
        let seed = envelope(
            &map,
            owner(),
            "seed",
            vec![
                MapOperation::AddNode {
                    node: node("A", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("B", AssertionOrigin::OwnerSpoken),
                },
            ],
        );
        let map1 = apply_ok(&map, &seed, TS);

        // Link (task, t1) to A ⇒ ok.
        let link_a = envelope(
            &map1,
            owner(),
            "l1",
            vec![MapOperation::LinkPromotedObject {
                node_id: "A".to_string(),
                promoted: PromotedRef {
                    destination_kind: PromotionKind::Task,
                    object_id: "t1".to_string(),
                    linked_at: TS.to_string(),
                },
            }],
        );
        let map2 = apply_ok(&map1, &link_a, TS2);
        assert_eq!(map2.nodes["A"].promoted_refs.len(), 1);

        // Link (task, t1) to B ⇒ conflict.
        let link_b = envelope(
            &map2,
            owner(),
            "l2",
            vec![MapOperation::LinkPromotedObject {
                node_id: "B".to_string(),
                promoted: PromotedRef {
                    destination_kind: PromotionKind::Task,
                    object_id: "t1".to_string(),
                    linked_at: TS.to_string(),
                },
            }],
        );
        let err = apply_envelope(&map2, &link_b, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::PromotionLinkConflict {
                destination_kind: "task".to_string(),
                object_id: "t1".to_string(),
            }
        );

        // Re-link same (A, task, t1) ⇒ idempotent, no dup.
        let relink = envelope(
            &map2,
            owner(),
            "l3",
            vec![MapOperation::LinkPromotedObject {
                node_id: "A".to_string(),
                promoted: PromotedRef {
                    destination_kind: PromotionKind::Task,
                    object_id: "t1".to_string(),
                    linked_at: TS.to_string(),
                },
            }],
        );
        let map3 = apply_ok(&map2, &relink, TS2);
        assert_eq!(map3.nodes["A"].promoted_refs.len(), 1);
    }

    // ── Confidence bounds ───────────────────────────────────────────────────────

    #[test]
    fn confidence_out_of_range_rejected() {
        let map = empty_map();
        let mut n = node("n1", AssertionOrigin::OwnerSpoken);
        n.confidence = 1.5;
        let env = envelope(&map, owner(), "i1", vec![MapOperation::AddNode { node: n }]);
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(err, ThinkingMapError::ConfidenceOutOfRange(1.5));
    }

    // ── Prompt-injection safety ─────────────────────────────────────────────────

    #[test]
    fn imported_cannot_command_privileged_ops() {
        let map = base_map_with_node();

        let confirm = envelope(
            &map,
            imported(),
            "p1",
            vec![MapOperation::ConfirmRestructure {
                proposal_id: "x".to_string(),
            }],
        );
        assert!(matches!(
            apply_envelope(&map, &confirm, TS2).unwrap_err(),
            ThinkingMapError::AuthorityViolation { .. }
        ));

        let link = envelope(
            &map,
            imported(),
            "p2",
            vec![MapOperation::LinkPromotedObject {
                node_id: "n1".to_string(),
                promoted: PromotedRef {
                    destination_kind: PromotionKind::Memory,
                    object_id: "m1".to_string(),
                    linked_at: TS.to_string(),
                },
            }],
        );
        assert!(matches!(
            apply_envelope(&map, &link, TS2).unwrap_err(),
            ThinkingMapError::AuthorityViolation { .. }
        ));
    }

    // ── Restructure confirm applies inner ops ───────────────────────────────────

    #[test]
    fn confirm_restructure_applies_inner_ops() {
        let map = empty_map();
        let proposal = RestructureProposal {
            proposal_id: "p1".to_string(),
            proposed_by: owner(),
            rationale: "add a node".to_string(),
            operations: vec![MapOperation::AddNode {
                node: node("inner", AssertionOrigin::OwnerSpoken),
            }],
            state: ProposalState::Proposed,
            affected_node_ids: vec![],
            created_at: TS.to_string(),
            resolved_at: None,
        };
        let propose = envelope(
            &map,
            owner(),
            "prop",
            vec![MapOperation::ProposeRestructure { proposal }],
        );
        let map1 = apply_ok(&map, &propose, TS);
        assert!(map1.proposals.contains_key("p1"));
        assert!(!map1.nodes.contains_key("inner"));

        let confirm = envelope(
            &map1,
            owner(),
            "conf",
            vec![MapOperation::ConfirmRestructure {
                proposal_id: "p1".to_string(),
            }],
        );
        let map2 = apply_ok(&map1, &confirm, TS2);
        // Inner node materialized; proposal Confirmed.
        assert!(map2.nodes.contains_key("inner"));
        assert_eq!(map2.proposals["p1"].state, ProposalState::Confirmed);
        assert_eq!(map2.proposals["p1"].resolved_at, Some(TS2.to_string()));
    }

    #[test]
    fn confirm_non_pending_proposal_rejected() {
        let map = empty_map();
        let proposal = RestructureProposal {
            proposal_id: "p1".to_string(),
            proposed_by: owner(),
            rationale: "noop".to_string(),
            operations: vec![],
            state: ProposalState::Proposed,
            affected_node_ids: vec![],
            created_at: TS.to_string(),
            resolved_at: None,
        };
        let map1 = apply_ok(
            &map,
            &envelope(
                &map,
                owner(),
                "prop",
                vec![MapOperation::ProposeRestructure { proposal }],
            ),
            TS,
        );
        // Reject first.
        let map2 = apply_ok(
            &map1,
            &envelope(
                &map1,
                owner(),
                "rej",
                vec![MapOperation::RejectRestructure {
                    proposal_id: "p1".to_string(),
                }],
            ),
            TS2,
        );
        assert_eq!(map2.proposals["p1"].state, ProposalState::Rejected);
        // Confirm now fails (not pending).
        let err = apply_envelope(
            &map2,
            &envelope(
                &map2,
                owner(),
                "conf",
                vec![MapOperation::ConfirmRestructure {
                    proposal_id: "p1".to_string(),
                }],
            ),
            TS2,
        )
        .unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::InvalidOperation("proposal_not_pending".to_string())
        );
    }

    // ── Semantic hash stability ─────────────────────────────────────────────────

    #[test]
    fn semantic_hash_order_independent() {
        // Build the same board via two different op orders that converge.
        let base = empty_map();

        // Order 1: add a then b.
        let env_a = envelope(
            &base,
            owner(),
            "o1",
            vec![
                MapOperation::AddNode {
                    node: node("a", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("b", AssertionOrigin::OwnerSpoken),
                },
            ],
        );
        let m1 = apply_ok(&base, &env_a, TS2);

        // Order 2: add b then a.
        let env_b = envelope(
            &base,
            owner(),
            "o2",
            vec![
                MapOperation::AddNode {
                    node: node("b", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("a", AssertionOrigin::OwnerSpoken),
                },
            ],
        );
        let m2 = apply_ok(&base, &env_b, TS2);

        assert_eq!(semantic_hash(&m1), semantic_hash(&m2));
    }

    #[test]
    fn semantic_hash_changes_on_label() {
        let map = base_map_with_node();
        let h0 = semantic_hash(&map);
        let env = envelope(
            &map,
            owner(),
            "u1",
            vec![MapOperation::UpdateNode {
                node_id: "n1".to_string(),
                label: Some("new label".to_string()),
                detail_markdown: None,
                confidence: None,
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        assert_ne!(h0, semantic_hash(&map1));
    }

    #[test]
    fn semantic_hash_ignores_view_state() {
        let map = base_map_with_node();
        let before = semantic_hash(&map);
        let env = envelope(
            &map,
            owner(),
            "v1",
            vec![MapOperation::SetSharedView {
                view_state: SharedViewState {
                    active_node: Some("n1".to_string()),
                    lens: ViewLens::Outline,
                },
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        // view_state changed AND revision bumped. Compare against the same
        // revision to isolate view_state: re-hash a projection with equal rev.
        // Here we assert view_state alone is excluded by building two maps that
        // differ ONLY in view_state at the same revision.
        let mut a = map1.clone();
        let mut b = map1.clone();
        a.view_state = SharedViewState::default();
        b.view_state = SharedViewState {
            active_node: Some("n1".to_string()),
            lens: ViewLens::Metrics,
        };
        assert_eq!(semantic_hash(&a), semantic_hash(&b));
        // And the hash DID change from before only because revision advanced.
        assert_ne!(before, semantic_hash(&map1));
    }

    #[test]
    fn semantic_hash_ignores_idempotency_ledger() {
        let map = base_map_with_node();
        let mut a = map.clone();
        let mut b = map.clone();
        a.applied_envelopes.clear();
        b.applied_envelopes.push_back(AppliedEnvelopeRecord {
            envelope_id: "extra".to_string(),
            idempotency_key: "extra".to_string(),
            resulting_revision: 99,
        });
        assert_eq!(semantic_hash(&a), semantic_hash(&b));
    }

    #[test]
    fn semantic_hash_includes_lifecycle() {
        let map = base_map_with_node();
        let mut a = map.clone();
        let mut b = map.clone();
        a.lifecycle = MapLifecycle::Active;
        b.lifecycle = MapLifecycle::Archived;
        assert_ne!(semantic_hash(&a), semantic_hash(&b));
    }

    // ── Determinism ─────────────────────────────────────────────────────────────

    #[test]
    fn determinism_byte_identical() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "i1",
            vec![
                MapOperation::AddNode {
                    node: node("n1", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("n2", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::Connect {
                    edge: edge(
                        "e1",
                        "n1",
                        "n2",
                        EdgeKind::Supports,
                        AssertionOrigin::OwnerSpoken,
                    ),
                },
            ],
        );
        let m1 = apply_ok(&map, &env, TS2);
        let m2 = apply_ok(&map, &env, TS2);
        let j1 = serde_json::to_vec(&m1).unwrap();
        let j2 = serde_json::to_vec(&m2).unwrap();
        assert_eq!(j1, j2);
        assert_eq!(semantic_hash(&m1), semantic_hash(&m2));
    }

    // ── rename_speaker ──────────────────────────────────────────────────────────

    #[test]
    fn rename_speaker_updates_display_names() {
        let map = empty_map();
        let mut n1 = node("n1", AssertionOrigin::OwnerSpoken);
        n1.speaker = Some(SpeakerRef {
            speaker_id: "spk-x".to_string(),
            display_name: Some("Old".to_string()),
        });
        let mut n2 = node("n2", AssertionOrigin::OwnerSpoken);
        n2.speaker = Some(SpeakerRef {
            speaker_id: "spk-y".to_string(),
            display_name: Some("Keep".to_string()),
        });
        let seed = envelope(
            &map,
            owner(),
            "seed",
            vec![
                MapOperation::AddNode { node: n1 },
                MapOperation::AddNode { node: n2 },
            ],
        );
        let map1 = apply_ok(&map, &seed, TS);
        let env = envelope(
            &map1,
            owner(),
            "rn",
            vec![MapOperation::RenameSpeaker {
                old_speaker_id: "spk-x".to_string(),
                new_display_name: "New".to_string(),
            }],
        );
        let map2 = apply_ok(&map1, &env, TS2);
        assert_eq!(
            map2.nodes["n1"].speaker.as_ref().unwrap().display_name,
            Some("New".to_string())
        );
        assert_eq!(
            map2.nodes["n2"].speaker.as_ref().unwrap().display_name,
            Some("Keep".to_string())
        );
    }

    // ── set_title / set_lifecycle (owner-only metadata) ──────────────────────────

    #[test]
    fn owner_set_title_changes_title_and_bumps_revision() {
        let map = empty_map();
        assert_eq!(map.title, "Test map");
        let env = envelope(
            &map,
            owner(),
            "t1",
            vec![MapOperation::SetTitle {
                title: "Renamed map".to_string(),
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        assert_eq!(map1.title, "Renamed map");
        assert_eq!(map1.revision, 1);
        assert_eq!(map1.updated_at, TS2);
    }

    #[test]
    fn owner_set_lifecycle_archives() {
        let map = empty_map();
        assert_eq!(map.lifecycle, MapLifecycle::Active);
        let env = envelope(
            &map,
            owner(),
            "l1",
            vec![MapOperation::SetLifecycle {
                lifecycle: MapLifecycle::Archived,
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        assert_eq!(map1.lifecycle, MapLifecycle::Archived);
        assert_eq!(map1.revision, 1);
    }

    #[test]
    fn model_set_title_is_authority_violation() {
        let map = empty_map();
        let env = envelope(
            &map,
            model(),
            "t1",
            vec![MapOperation::SetTitle {
                title: "hijack".to_string(),
            }],
        );
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert!(
            matches!(err, ThinkingMapError::AuthorityViolation { .. }),
            "expected AuthorityViolation, got {err:?}"
        );
    }

    #[test]
    fn model_set_lifecycle_is_authority_violation() {
        let map = empty_map();
        let env = envelope(
            &map,
            model(),
            "l1",
            vec![MapOperation::SetLifecycle {
                lifecycle: MapLifecycle::Archived,
            }],
        );
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert!(
            matches!(err, ThinkingMapError::AuthorityViolation { .. }),
            "expected AuthorityViolation, got {err:?}"
        );
    }

    #[test]
    fn set_title_set_lifecycle_not_model_accessible() {
        assert!(!MapOperation::SetTitle {
            title: "x".to_string(),
        }
        .is_model_accessible());
        assert!(!MapOperation::SetLifecycle {
            lifecycle: MapLifecycle::Archived,
        }
        .is_model_accessible());
    }

    #[test]
    fn owner_set_title_empty_is_invalid_operation() {
        let map = empty_map();
        let env = envelope(
            &map,
            owner(),
            "t1",
            vec![MapOperation::SetTitle {
                title: "   ".to_string(),
            }],
        );
        let err = apply_envelope(&map, &env, TS2).unwrap_err();
        assert_eq!(
            err,
            ThinkingMapError::InvalidOperation("empty_title".to_string())
        );
    }

    // ── ledger bound ────────────────────────────────────────────────────────────

    #[test]
    fn idempotency_ledger_is_bounded() {
        let mut map = empty_map();
        // Simulate a full ledger + apply one more; should stay at cap.
        for i in 0..APPLIED_ENVELOPE_LEDGER_CAP {
            map.applied_envelopes.push_back(AppliedEnvelopeRecord {
                envelope_id: format!("old-{i}"),
                idempotency_key: format!("oldkey-{i}"),
                resulting_revision: 0,
            });
        }
        let env = envelope(
            &map,
            owner(),
            "fresh",
            vec![MapOperation::AddNode {
                node: node("n1", AssertionOrigin::OwnerSpoken),
            }],
        );
        let map1 = apply_ok(&map, &env, TS2);
        assert_eq!(map1.applied_envelopes.len(), APPLIED_ENVELOPE_LEDGER_CAP);
        // Newest record present, oldest evicted.
        assert!(map1
            .applied_envelopes
            .iter()
            .any(|r| r.envelope_id == "env-fresh"));
        assert!(!map1
            .applied_envelopes
            .iter()
            .any(|r| r.envelope_id == "old-0"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Restructure authority: no privilege laundering (security regressions)
    // ═══════════════════════════════════════════════════════════════════════

    /// A restructure cannot launder an owner-only op past an owner confirm. A
    /// `model` (untrusted) proposes a restructure whose inner op is the
    /// OWNER-ONLY `link_promoted_object`. The reducer validates inner ops against
    /// the PROPOSING actor at propose time, so the proposal is rejected up front
    /// and can never be staged for a later one-click confirm. A client-supplied
    /// `proposed_by: owner` does not help — it is ignored/overwritten.
    #[test]
    fn model_cannot_launder_owner_only_link_via_restructure() {
        let map = base_map_with_node();
        let malicious_proposal = RestructureProposal {
            proposal_id: "p-evil".to_string(),
            // Attacker tries to self-elevate by claiming an owner proposer; the
            // reducer ignores this and validates against the ENVELOPE actor.
            proposed_by: owner(),
            rationale: "totally benign, just reorganizing".to_string(),
            operations: vec![MapOperation::LinkPromotedObject {
                node_id: "n1".to_string(),
                promoted: PromotedRef {
                    destination_kind: PromotionKind::Task,
                    object_id: "attacker-task".to_string(),
                    linked_at: TS.to_string(),
                },
            }],
            state: ProposalState::Proposed,
            affected_node_ids: vec![],
            created_at: TS.to_string(),
            resolved_at: None,
        };
        let propose = envelope(
            &map,
            model(), // untrusted model author
            "prop-evil",
            vec![MapOperation::ProposeRestructure {
                proposal: malicious_proposal,
            }],
        );
        // Rejected at propose time: a model may not emit link_promoted_object, so
        // it can never even be staged inside a proposal.
        let err = apply_envelope(&map, &propose, TS).unwrap_err();
        assert!(
            matches!(err, ThinkingMapError::AuthorityViolation { .. }),
            "expected AuthorityViolation, got {err:?}"
        );
        // Atomic: nothing staged, no link created.
        assert!(!map.proposals.contains_key("p-evil"));
        assert!(map.nodes["n1"].promoted_refs.is_empty());
    }

    /// A model MAY propose a legitimate STRUCTURAL restructure (only ops it can
    /// emit). Owner confirm applies the inner ops under the MODEL's authority, so
    /// the added node keeps its honest `model_inferred` provenance — the very
    /// behavior blanket-owner application would have wrongly rejected.
    #[test]
    fn model_structural_restructure_confirm_preserves_provenance() {
        let base = empty_map();
        let seed = envelope(
            &base,
            owner(),
            "seed",
            vec![
                MapOperation::AddNode {
                    node: node("n1", AssertionOrigin::OwnerSpoken),
                },
                MapOperation::AddNode {
                    node: node("n2", AssertionOrigin::OwnerSpoken),
                },
            ],
        );
        let map = apply_ok(&base, &seed, TS);

        // Model proposes: add a group node (model_inferred) + group n1 under it.
        let mut group = node("g1", AssertionOrigin::ModelInferred);
        group.kind = NodeKind::Group;
        let proposal = RestructureProposal {
            proposal_id: "p-group".to_string(),
            proposed_by: owner(), // ignored; overwritten with the model author
            rationale: "group these".to_string(),
            operations: vec![
                MapOperation::AddNode { node: group },
                MapOperation::Connect {
                    edge: edge(
                        "e1",
                        "n1",
                        "g1",
                        EdgeKind::GroupedUnder,
                        AssertionOrigin::ModelInferred,
                    ),
                },
            ],
            state: ProposalState::Proposed,
            affected_node_ids: vec!["n1".to_string()],
            created_at: TS.to_string(),
            resolved_at: None,
        };
        let map1 = apply_ok(
            &map,
            &envelope(
                &map,
                model(),
                "prop",
                vec![MapOperation::ProposeRestructure { proposal }],
            ),
            TS,
        );
        // Stamped with the real proposer; not applied until confirm.
        assert!(matches!(
            map1.proposals["p-group"].proposed_by,
            OperationActor::Model { .. }
        ));
        assert!(!map1.nodes.contains_key("g1"));

        let map2 = apply_ok(
            &map1,
            &envelope(
                &map1,
                owner(),
                "conf",
                vec![MapOperation::ConfirmRestructure {
                    proposal_id: "p-group".to_string(),
                }],
            ),
            TS2,
        );
        assert!(map2.nodes.contains_key("g1"));
        // Provenance preserved — NOT rewritten to owner.
        assert_eq!(
            map2.nodes["g1"].assertion_origin,
            AssertionOrigin::ModelInferred
        );
        assert!(map2.edges.contains_key("e1"));
        assert_eq!(map2.proposals["p-group"].state, ProposalState::Confirmed);
    }

    /// Variant — the same protection blocks an owner-only `rename_speaker`
    /// smuggled into a model-authored proposal: rejected at propose time.
    #[test]
    fn model_cannot_launder_rename_speaker_via_restructure() {
        let mut n1 = node("n1", AssertionOrigin::OwnerSpoken);
        n1.speaker = Some(SpeakerRef {
            speaker_id: "spk-x".to_string(),
            display_name: Some("RealName".to_string()),
        });
        let base = empty_map();
        let seed = envelope(
            &base,
            owner(),
            "seed",
            vec![MapOperation::AddNode { node: n1 }],
        );
        let map = apply_ok(&base, &seed, TS);

        let proposal = RestructureProposal {
            proposal_id: "p-rn".to_string(),
            proposed_by: owner(),
            rationale: "cleanup".to_string(),
            operations: vec![MapOperation::RenameSpeaker {
                old_speaker_id: "spk-x".to_string(),
                new_display_name: "HIJACKED".to_string(),
            }],
            state: ProposalState::Proposed,
            affected_node_ids: vec![],
            created_at: TS.to_string(),
            resolved_at: None,
        };
        let propose = envelope(
            &map,
            model(),
            "prop-rn",
            vec![MapOperation::ProposeRestructure { proposal }],
        );
        let err = apply_envelope(&map, &propose, TS).unwrap_err();
        assert!(
            matches!(err, ThinkingMapError::AuthorityViolation { .. }),
            "expected AuthorityViolation, got {err:?}"
        );
        // Speaker name untouched; proposal never staged.
        assert!(!map.proposals.contains_key("p-rn"));
        assert_eq!(
            map.nodes["n1"].speaker.as_ref().unwrap().display_name,
            Some("RealName".to_string())
        );
    }
}
