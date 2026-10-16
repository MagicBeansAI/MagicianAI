//
//  CanonicalMapImporter.swift
//  Magios
//
//  S2d/S2e — the two PURE, UNGATED pieces that back the canonical library:
//
//    1. `CanonicalMapLibraryProjection` — projects a server `LTM.Summary` (plus
//       the client-local prefs) down onto E0's `ThinkingMapRecord` so the model's
//       `@Published maps` can be sourced from `client.listMaps()` under the flag.
//
//    2. `CanonicalMapImportPlanner` — the one-time `UserDefaults`→canonical
//       importer's op-building, factored into a pure
//       `static func importOps(for:) -> ImportPlan` (create + N add_node +
//       move_to_parent + connect). Deterministic and unit-testable with no
//       backend; the gated runner (in `CanonicalThinkingMapBackend`) drives it.
//
//  Both are ungated (always compiled) so the flag-OFF build type-checks + tests
//  them. Neither performs IO.
//

import Foundation

// MARK: - Library projection: LTM.Summary + local prefs → ThinkingMapRecord

enum CanonicalMapLibraryProjection {
    /// Project one server summary + its client-local prefs onto an E0
    /// `ThinkingMapRecord`. The record's `snapshot` carries a LIGHTWEIGHT node
    /// preview (from `summary.nodePreview`, when the server sent one) so the
    /// library card's `ThinkingMapMiniGraph` can draw a thumbnail + show real
    /// node/question/action counts — the full authoritative graph is still
    /// fetched + projected on OPEN. When the summary has no preview (older server
    /// / empty map) the snapshot's nodes/edges stay empty exactly as before.
    ///
    ///   - id            ← `summary.mapId` (via a stable UUID resolution)
    ///   - title         ← `summary.title`
    ///   - isArchived    ← `summary.lifecycle == .archived`
    ///   - updatedAt     ← `summary.updatedAt`
    ///   - isPinned      ← local prefs
    ///   - preferredMode ← local prefs
    ///   - lastOpenedAt  ← local prefs, falling back to `updatedAt`
    ///   - snapshot.nodes/edges ← `summary.nodePreview` (bounded; may be empty)
    static func record(
        from summary: LTM.Summary,
        pref: CanonicalMapLocalPref
    ) -> ThinkingMapRecord {
        let id = resolveUUID(summary.mapId)
        let updatedAt = parseTimestamp(summary.updatedAt)
        let lastOpened = pref.lastOpenedAt ?? updatedAt
        let (previewNodes, previewEdges) = projectPreview(summary.nodePreview, createdAt: updatedAt)
        let snapshot = ThinkingMapSnapshot(
            title: summary.title,
            nodes: previewNodes,
            edges: previewEdges,
            activeNodeID: nil)
        return ThinkingMapRecord(
            id: id,
            createdAt: updatedAt,
            updatedAt: updatedAt,
            lastOpenedAt: lastOpened,
            isPinned: pref.isPinned,
            isArchived: summary.lifecycle == .archived,
            preferredMode: pref.preferredMode,
            brainstormSessionID: nil,
            snapshot: snapshot
        )
    }

    /// Project a server `LTM.NodePreview` (bounded thumbnail) onto E0 view types
    /// so the library-card mini-graph can render. Returns `([], [])` when the
    /// preview is absent (older server / empty map). Node/edge ids resolve
    /// through the SAME `resolveUUID` on both endpoints, so a preview branch
    /// edge's `from`/`to` line up with the projected node ids. Kind maps via the
    /// shared `thinkingKind(from:)`; `suggested` carries straight through. The
    /// non-graph fields (`detail`/`source`/`revision`/`promotedKinds`) are
    /// placeholders — the mini-graph only reads `id`, `parentID`, and `kind`.
    private static func projectPreview(
        _ preview: LTM.NodePreview?,
        createdAt: Date
    ) -> (nodes: [ThinkingNode], edges: [ThinkingEdge]) {
        guard let preview else { return ([], []) }
        let nodes: [ThinkingNode] = preview.nodes.map { node in
            ThinkingNode(
                id: resolveUUID(node.nodeId),
                parentID: node.parentId.map(resolveUUID),
                kind: CanonicalThinkingMapProjection.thinkingKind(from: node.kind),
                title: node.title,
                detail: "",
                source: node.suggested ? "model_inferred" : "owner_spoken",
                suggested: node.suggested,
                revision: 1,
                createdAt: createdAt,
                promotedKinds: []
            )
        }
        let edges: [ThinkingEdge] = preview.edges.map { edge in
            ThinkingEdge(
                id: CanonicalThinkingMapProjection.deterministicUUID(from: "branch:\(edge.to)"),
                from: resolveUUID(edge.from),
                to: resolveUUID(edge.to),
                kind: .branch
            )
        }
        return (nodes, edges)
    }

    /// A canonical map-id string → the UUID E0's `ThinkingMapRecord` requires.
    /// Real canonical ids are already UUID strings; anything else gets the same
    /// deterministic fallback the node projection uses, so ids stay stable.
    static func resolveUUID(_ id: String) -> UUID {
        UUID(uuidString: id) ?? CanonicalThinkingMapProjection.deterministicUUID(from: id)
    }

    /// Parse a canonical RFC3339 timestamp, tolerating BOTH fractional-second
    /// (`...T12:00:00.123Z`) and whole-second (`...T12:00:00Z`) forms; falls back
    /// to a stable epoch so ordering stays deterministic when unparseable.
    private static func parseTimestamp(_ string: String) -> Date {
        Self.fractionalParser.date(from: string)
            ?? Self.wholeParser.date(from: string)
            ?? Date(timeIntervalSince1970: 0)
    }

    private static let fractionalParser: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private static let wholeParser: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()
}

// MARK: - Importer op-building (S2e), pure + unit-testable

/// The plan for importing ONE E0 `ThinkingMapRecord` into the canonical backend:
/// the create-map parameters plus the owner operations that replay the local
/// map's content (nodes + parent links + related edges).
struct CanonicalMapImportPlan: Equatable {
    /// The title for the new canonical map.
    let title: String
    /// The canonical map id to create with (the local UUID string, so the same
    /// local map always maps to the same canonical id — re-running is a no-op).
    let mapID: String
    /// The owner operations to apply after creation, in order.
    let operations: [LTM.Operation]
}

enum CanonicalMapImportPlanner {
    /// Build the import plan for a local record. Every node is imported as OWNER
    /// content (owner_spoken / asserted) — E0's "suggested" nodes were
    /// user-accepted into the saved map, so they are the user's content now.
    ///
    /// Ops, in order:
    ///   - one `.addNode` per local node (owner_spoken / asserted, kind preserved)
    ///   - one `.moveToParent` per node that has a resolvable parent (mirrors the
    ///     local tree; branch edges are implicit via parent_id, NOT re-added)
    ///   - one `.connect` per local `.related` edge with both endpoints present
    ///
    /// Local UUIDs are reused verbatim as canonical node/edge ids so parent links
    /// and cross-links resolve, and so a re-run would mint identical ids.
    static func importOps(for record: ThinkingMapRecord, now: Date = Date()) -> CanonicalMapImportPlan {
        let stamp = CanonicalThinkingMapOps.timestamp(now)
        let snapshot = record.snapshot
        let nodeIDs = Set(snapshot.nodes.map { $0.id })

        var ops: [LTM.Operation] = []

        // 1. add_node for every local node (owner content).
        for node in snapshot.nodes {
            let canonical = LTM.Node(
                nodeId: node.id.uuidString,
                kind: CanonicalThinkingMapOps.canonicalKind(node.kind),
                label: node.title,
                detailMarkdown: node.detail.isEmpty ? nil : node.detail,
                epistemicState: .asserted,
                assertionOrigin: .ownerSpoken,
                confidence: 1.0,
                parentId: parentID(of: node, in: nodeIDs),
                createdAt: stamp,
                updatedAt: stamp
            )
            ops.append(.addNode(node: canonical))
        }

        // 2. move_to_parent to mirror the local tree (parent must be a live node).
        for node in snapshot.nodes {
            if let parent = parentID(of: node, in: nodeIDs) {
                ops.append(.moveToParent(nodeId: node.id.uuidString, parentId: parent))
            }
        }

        // 3. connect for each related cross-link with both endpoints present.
        //    Branch edges are implicit via parent_id and are intentionally skipped.
        for edge in snapshot.edges where edge.kind == .related
            && nodeIDs.contains(edge.from) && nodeIDs.contains(edge.to) {
            let canonicalEdge = LTM.Edge(
                edgeId: edge.id.uuidString,
                fromNode: edge.from.uuidString,
                toNode: edge.to.uuidString,
                kind: .relatedTo,
                assertionOrigin: .ownerSpoken,
                createdAt: stamp,
                updatedAt: stamp
            )
            ops.append(.connect(edge: canonicalEdge))
        }

        return CanonicalMapImportPlan(
            title: record.title,
            mapID: record.id.uuidString,
            operations: ops
        )
    }

    /// A node's parent id (as a canonical string), but only when the parent is a
    /// live node in the same snapshot — mirrors the projection's live-parent rule.
    private static func parentID(of node: ThinkingNode, in liveIDs: Set<UUID>) -> String? {
        guard let parent = node.parentID, liveIDs.contains(parent) else { return nil }
        return parent.uuidString
    }
}
