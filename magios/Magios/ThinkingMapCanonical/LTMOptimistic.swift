//  LTMOptimistic.swift
//  Live Thinking Map (LTM) — S1c: the COSMETIC optimistic transform.
//
//  ⚠️  NON-AUTHORITATIVE PREVIEW. ⚠️
//
//  This is NOT a second reducer. It applies a small subset of the common owner
//  operations to a *local copy* of the map purely so the UI can update the
//  instant a capture happens, before the round-trip to the Rust reducer (the
//  single source of truth) completes. It:
//
//    - performs NO authority / permission checks,
//    - does NOT bump `revision` (the server owns the revision counter),
//    - does NOT validate references, enforce invariants, or fork semantics,
//    - SKIPS any operation it does not recognize (the server response fills it
//      in), and is therefore intentionally lossy.
//
//  The `SyncStore` ALWAYS replaces the optimistically-transformed map with the
//  authoritative `LTM.Map` the server returns from `applyOperations`. If this
//  preview ever disagrees with the server, the server wins — unconditionally.
//  Treat every line below as "best-effort cosmetics", never as truth.

import Foundation

extension LTM {
    /// Cosmetic, non-authoritative preview transform. Mutates `map` in place by
    /// applying the recognized subset of `ops`. See the file header: this is a
    /// UI-only preview that the server's authoritative response overwrites.
    ///
    /// Recognized ops: `add_node`, `connect`, `update_node` (label / detail via
    /// `FieldEdit` / confidence), `set_node_kind`, `set_epistemic_state`,
    /// `tombstone_node`, `move_to_parent`, `create_clarification`, `set_title`,
    /// `set_lifecycle`. Everything else is skipped.
    public static func optimisticApply(_ map: inout LTM.Map, _ ops: [LTM.Operation]) {
        for op in ops {
            optimisticApplyOne(&map, op)
        }
    }

    /// Apply ONE recognized op to the local preview map. Unknown ops are no-ops.
    private static func optimisticApplyOne(_ map: inout LTM.Map, _ op: LTM.Operation) {
        switch op {
        case let .addNode(node):
            // Insert (or replace) the node under its id.
            map.nodes[node.nodeId] = node

        case let .connect(edge):
            map.edges[edge.edgeId] = edge

        case let .updateNode(nodeId, label, detailMarkdown, confidence):
            guard var node = map.nodes[nodeId] else { break }
            if let label { node.label = label }
            switch detailMarkdown {
            case .unchanged:
                break
            case .clear:
                node.detailMarkdown = nil
            case let .set(value):
                node.detailMarkdown = value
            }
            if let confidence { node.confidence = confidence }
            map.nodes[nodeId] = node

        case let .setNodeKind(nodeId, kind):
            guard var node = map.nodes[nodeId] else { break }
            node.kind = kind
            map.nodes[nodeId] = node

        case let .setEpistemicState(nodeId, state):
            guard var node = map.nodes[nodeId] else { break }
            node.epistemicState = state
            map.nodes[nodeId] = node

        case let .tombstoneNode(nodeId):
            guard var node = map.nodes[nodeId] else { break }
            node.tombstoned = true
            map.nodes[nodeId] = node

        case let .moveToParent(nodeId, parentId):
            guard var node = map.nodes[nodeId] else { break }
            node.parentId = parentId
            map.nodes[nodeId] = node

        case let .createClarification(clarification):
            map.clarifications[clarification.clarificationId] = clarification

        case let .setTitle(title):
            map.title = title

        case let .setLifecycle(lifecycle):
            map.lifecycle = lifecycle

        // Everything else is intentionally NOT previewed — the server's
        // authoritative response is the source of truth for these.
        case .restoreNode, .disconnect, .moveNode, .setPositionLock,
             .resolveClarification, .proposeRestructure, .confirmRestructure,
             .rejectRestructure, .setSharedView, .linkPromotedObject,
             .unlinkPromotedObject, .renameSpeaker:
            break
        }
    }
}
