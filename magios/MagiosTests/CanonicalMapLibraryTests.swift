//
//  CanonicalMapLibraryTests.swift
//  Verifies the PURE, UNGATED pieces of the S2d/S2e canonical library work:
//    - `CanonicalMapLibraryProjection` (LTM.Summary + local prefs → record)
//    - `CanonicalMapImportPlanner.importOps` (record → create + ops + idempotency)
//    - `CanonicalMapLocalPrefs` round-trip (pin / mode / import ledger)
//
//  All run in the normal (flag-OFF) build — no live backend or gated runtime.
//

import XCTest
@testable import Magician

final class CanonicalMapLibraryTests: XCTestCase {

    // MARK: - Summary → ThinkingMapRecord projection

    private func summary(
        id: String = "11111111-1111-1111-1111-111111111111",
        title: String = "Map A",
        lifecycle: LTM.MapLifecycle = .active,
        revision: UInt64 = 3,
        updatedAt: String = "2026-07-20T12:00:00Z",
        preview: LTM.NodePreview? = nil
    ) -> LTM.Summary {
        LTM.Summary(
            mapId: id, title: title, lifecycle: lifecycle,
            latestRevision: revision, updatedAt: updatedAt,
            nodePreview: preview)
    }

    func testRecordProjectionTakesTitleAndArchivedFromSummary() {
        let record = CanonicalMapLibraryProjection.record(
            from: summary(lifecycle: .archived),
            pref: CanonicalMapLocalPref())
        XCTAssertEqual(record.title, "Map A")
        XCTAssertTrue(record.isArchived)          // lifecycle == .archived
        XCTAssertFalse(record.isPinned)           // default pref
        XCTAssertEqual(record.preferredMode, .map) // default pref
    }

    func testRecordProjectionActiveIsNotArchived() {
        let record = CanonicalMapLibraryProjection.record(
            from: summary(lifecycle: .active), pref: CanonicalMapLocalPref())
        XCTAssertFalse(record.isArchived)
    }

    func testRecordProjectionMergesLocalPrefs() {
        let opened = Date(timeIntervalSince1970: 1_800_000_000)
        let pref = CanonicalMapLocalPref(isPinned: true, preferredMode: .outline, lastOpenedAt: opened)
        let record = CanonicalMapLibraryProjection.record(from: summary(), pref: pref)
        XCTAssertTrue(record.isPinned)
        XCTAssertEqual(record.preferredMode, .outline)
        XCTAssertEqual(record.lastOpenedAt, opened)
    }

    func testRecordProjectionFallsBackLastOpenedToUpdatedAt() {
        // No local lastOpened → fall back to the summary's updatedAt.
        let record = CanonicalMapLibraryProjection.record(
            from: summary(updatedAt: "2026-07-20T12:00:00Z"),
            pref: CanonicalMapLocalPref(lastOpenedAt: nil))
        XCTAssertEqual(record.lastOpenedAt, record.updatedAt)
        XCTAssertNotEqual(record.updatedAt, Date(timeIntervalSince1970: 0))
    }

    func testRecordProjectionUUIDIsStableFromMapId() {
        let uuidStr = "22222222-2222-2222-2222-222222222222"
        let record = CanonicalMapLibraryProjection.record(
            from: summary(id: uuidStr), pref: CanonicalMapLocalPref())
        XCTAssertEqual(record.id, UUID(uuidString: uuidStr))
    }

    // MARK: - Node preview → snapshot projection (library-card mini-graph)

    func testRecordProjectionWithoutPreviewHasEmptySnapshot() {
        // No node_preview (older server / empty map) ⇒ empty nodes/edges, as before.
        let record = CanonicalMapLibraryProjection.record(
            from: summary(), pref: CanonicalMapLocalPref())
        XCTAssertTrue(record.snapshot.nodes.isEmpty)
        XCTAssertTrue(record.snapshot.edges.isEmpty)
    }

    func testRecordProjectionPopulatesSnapshotFromPreview() {
        let rootID = "33333333-3333-3333-3333-333333333333"
        let childID = "44444444-4444-4444-4444-444444444444"
        let preview = LTM.NodePreview(
            nodes: [
                LTM.NodePreviewNode(
                    nodeId: rootID, parentId: nil, kind: .idea,
                    suggested: false, title: "Root idea"),
                LTM.NodePreviewNode(
                    nodeId: childID, parentId: rootID, kind: .question,
                    suggested: true, title: "Open question"),
            ],
            edges: [LTM.NodePreviewEdge(from: rootID, to: childID)])
        let record = CanonicalMapLibraryProjection.record(
            from: summary(preview: preview), pref: CanonicalMapLocalPref())

        // Nodes projected with resolved UUIDs, kinds, and suggested flags.
        XCTAssertEqual(record.snapshot.nodes.count, 2)
        let root = record.snapshot.nodes[0]
        let child = record.snapshot.nodes[1]
        XCTAssertEqual(root.id, UUID(uuidString: rootID))
        XCTAssertNil(root.parentID)
        XCTAssertEqual(root.kind, .idea)
        XCTAssertFalse(root.suggested)
        XCTAssertEqual(child.id, UUID(uuidString: childID))
        XCTAssertEqual(child.parentID, UUID(uuidString: rootID))
        XCTAssertEqual(child.kind, .question)
        XCTAssertTrue(child.suggested)

        // Branch edge endpoints line up 1:1 with the projected node ids so the
        // mini-graph can position them.
        XCTAssertEqual(record.snapshot.edges.count, 1)
        let edge = record.snapshot.edges[0]
        XCTAssertEqual(edge.kind, .branch)
        XCTAssertEqual(edge.from, root.id)
        XCTAssertEqual(edge.to, child.id)

        // Derived library-card counts now reflect the preview (were 0 before).
        XCTAssertEqual(record.nodeCount, 2)
        XCTAssertEqual(record.questionCount, 1)
    }

    func testRecordProjectionUUIDIsDeterministicForNonUUIDMapId() {
        let a = CanonicalMapLibraryProjection.resolveUUID("map-seed-1")
        let b = CanonicalMapLibraryProjection.resolveUUID("map-seed-1")
        let c = CanonicalMapLibraryProjection.resolveUUID("map-seed-2")
        XCTAssertEqual(a, b)
        XCTAssertNotEqual(a, c)
    }

    // MARK: - Importer op-building

    /// A sample local record: 2 nodes under a root + a related cross-link.
    ///   root (idea)  — owner
    ///     child (question) — owner, parentID = root
    ///   related edge root ↔ child
    private func sampleRecord() -> (ThinkingMapRecord, root: UUID, child: UUID) {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        let rootID = UUID(uuidString: "AAAAAAAA-0000-0000-0000-000000000001")!
        let childID = UUID(uuidString: "AAAAAAAA-0000-0000-0000-000000000002")!
        let root = ThinkingNode(
            id: rootID, parentID: nil, kind: .idea, title: "Root idea",
            detail: "Root detail", source: "seed", suggested: false, revision: 1, createdAt: now)
        let child = ThinkingNode(
            id: childID, parentID: rootID, kind: .question, title: "A question",
            detail: "", source: "seed", suggested: true, revision: 1, createdAt: now)
        let related = ThinkingEdge(id: UUID(), from: rootID, to: childID, kind: .related)
        let branch = ThinkingEdge(id: UUID(), from: rootID, to: childID, kind: .branch)
        let snapshot = ThinkingMapSnapshot(
            title: "Sample", nodes: [root, child], edges: [branch, related], activeNodeID: rootID)
        let record = ThinkingMapRecord(
            id: UUID(uuidString: "BBBBBBBB-0000-0000-0000-000000000001")!,
            createdAt: now, updatedAt: now, lastOpenedAt: now,
            isPinned: false, isArchived: false, preferredMode: .map,
            brainstormSessionID: nil, snapshot: snapshot)
        return (record, rootID, childID)
    }

    func testImportOpsBuildsCreateThenAddNodesMoveAndConnect() {
        let (record, rootID, childID) = sampleRecord()
        let plan = CanonicalMapImportPlanner.importOps(for: record)

        // Map id is the local uuid string → re-runs collide + are skipped.
        XCTAssertEqual(plan.mapID, record.id.uuidString)
        XCTAssertEqual(plan.title, "Sample")

        // 2 add_node + 1 move_to_parent (child only) + 1 connect = 4 ops.
        XCTAssertEqual(plan.operations.count, 4)

        // add_node: both nodes, imported as owner_spoken / asserted.
        let adds = plan.operations.compactMap { op -> LTM.Node? in
            if case let .addNode(node) = op { return node }
            return nil
        }
        XCTAssertEqual(adds.count, 2)
        for node in adds {
            XCTAssertEqual(node.assertionOrigin, .ownerSpoken)   // even the "suggested" child
            XCTAssertEqual(node.epistemicState, .asserted)
        }
        let rootNode = adds.first { $0.nodeId == rootID.uuidString }
        XCTAssertEqual(rootNode?.kind, .idea)
        XCTAssertEqual(rootNode?.label, "Root idea")
        XCTAssertEqual(rootNode?.detailMarkdown, "Root detail")
        XCTAssertNil(rootNode?.parentId)
        let childNode = adds.first { $0.nodeId == childID.uuidString }
        XCTAssertEqual(childNode?.kind, .question)
        XCTAssertEqual(childNode?.parentId, rootID.uuidString)
        XCTAssertNil(childNode?.detailMarkdown)  // empty detail → nil

        // exactly one move_to_parent, for the child → root.
        let moves = plan.operations.compactMap { op -> (String, String?)? in
            if case let .moveToParent(nodeId, parentId) = op { return (nodeId, parentId) }
            return nil
        }
        XCTAssertEqual(moves.count, 1)
        XCTAssertEqual(moves.first?.0, childID.uuidString)
        XCTAssertEqual(moves.first?.1, rootID.uuidString)

        // exactly one connect for the related edge; the branch edge is NOT re-added.
        let connects = plan.operations.compactMap { op -> LTM.Edge? in
            if case let .connect(edge) = op { return edge }
            return nil
        }
        XCTAssertEqual(connects.count, 1)
        XCTAssertEqual(connects.first?.kind, .relatedTo)
        XCTAssertEqual(Set([connects.first?.fromNode, connects.first?.toNode]),
                       Set([rootID.uuidString, childID.uuidString]))
    }

    func testImportOpsForEmptySnapshotHasNoOperations() {
        let empty = ThinkingMapRecord(
            id: UUID(), createdAt: Date(), updatedAt: Date(), lastOpenedAt: Date(),
            isPinned: false, isArchived: false, preferredMode: .map, brainstormSessionID: nil,
            snapshot: ThinkingMapSnapshot(title: "Empty", nodes: [], edges: [], activeNodeID: nil))
        let plan = CanonicalMapImportPlanner.importOps(for: empty)
        XCTAssertTrue(plan.operations.isEmpty)
        XCTAssertEqual(plan.title, "Empty")
    }

    func testImportOpsIsDeterministicForSameRecord() {
        let (record, _, _) = sampleRecord()
        let fixed = Date(timeIntervalSince1970: 1_700_000_000)
        let a = CanonicalMapImportPlanner.importOps(for: record, now: fixed)
        let b = CanonicalMapImportPlanner.importOps(for: record, now: fixed)
        XCTAssertEqual(a, b)   // same record + same clock → identical plan
    }

    // MARK: - Local prefs round-trip + idempotency ledger

    private func freshPrefs() -> CanonicalMapLocalPrefs {
        let suite = "test-canonical-localprefs-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return CanonicalMapLocalPrefs(defaults: defaults, key: "prefs.test")
    }

    func testLocalPrefsPinRoundTrips() {
        let store = freshPrefs()
        XCTAssertFalse(store.isPinned("m1"))
        XCTAssertTrue(store.togglePinned("m1"))
        XCTAssertTrue(store.isPinned("m1"))
        XCTAssertFalse(store.togglePinned("m1"))
        XCTAssertFalse(store.isPinned("m1"))
    }

    func testLocalPrefsModeRoundTrips() {
        let store = freshPrefs()
        XCTAssertEqual(store.preferredMode("m1"), .map)  // default
        store.setPreferredMode(.focus, for: "m1")
        XCTAssertEqual(store.preferredMode("m1"), .focus)
    }

    func testLocalPrefsPersistAcrossInstances() {
        let suite = "test-canonical-localprefs-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        let key = "prefs.persist"
        let a = CanonicalMapLocalPrefs(defaults: defaults, key: key)
        a.togglePinned("m9")
        a.setPreferredMode(.outline, for: "m9")

        let b = CanonicalMapLocalPrefs(defaults: defaults, key: key)
        XCTAssertTrue(b.isPinned("m9"))
        XCTAssertEqual(b.preferredMode("m9"), .outline)
    }

    func testImportLedgerIsIdempotent() {
        let store = freshPrefs()
        XCTAssertFalse(store.isImported(localMapID: "local-1"))
        store.markImported(localMapID: "local-1")
        store.markImported(localMapID: "local-1")   // idempotent
        XCTAssertTrue(store.isImported(localMapID: "local-1"))
        XCTAssertEqual(store.importedLocalMapIDs, ["local-1"])
    }

    func testForgetDropsPrefs() {
        let store = freshPrefs()
        store.togglePinned("m1")
        XCTAssertTrue(store.isPinned("m1"))
        store.forget("m1")
        XCTAssertFalse(store.isPinned("m1"))   // back to defaults
    }

    // MARK: - Share "Add to current Thinking Map" append target (pure)

    private func record(
        id: String,
        lastOpened: TimeInterval,
        archived: Bool = false,
        pinned: Bool = false
    ) -> ThinkingMapRecord {
        let opened = Date(timeIntervalSince1970: lastOpened)
        return ThinkingMapRecord(
            id: UUID(uuidString: id)!,
            createdAt: opened, updatedAt: opened, lastOpenedAt: opened,
            isPinned: pinned, isArchived: archived, preferredMode: .map,
            brainstormSessionID: nil,
            snapshot: ThinkingMapSnapshot(title: "T", nodes: [], edges: [], activeNodeID: nil))
    }

    func testAppendTargetPicksMostRecentlyOpened() {
        let older = record(id: "CCCCCCCC-0000-0000-0000-000000000001", lastOpened: 1_000)
        let newer = record(id: "CCCCCCCC-0000-0000-0000-000000000002", lastOpened: 2_000)
        let target = ThinkingMapModel.mostRecentAppendTarget(in: [older, newer])
        XCTAssertEqual(target?.id, newer.id)
    }

    func testAppendTargetSkipsArchivedMaps() {
        let archived = record(id: "CCCCCCCC-0000-0000-0000-000000000003", lastOpened: 9_000, archived: true)
        let active = record(id: "CCCCCCCC-0000-0000-0000-000000000004", lastOpened: 1_000)
        let target = ThinkingMapModel.mostRecentAppendTarget(in: [archived, active])
        XCTAssertEqual(target?.id, active.id, "an archived map is never the append target")
    }

    func testAppendTargetIgnoresPinInFavorOfRecency() {
        // A pin is a library-display concern; "current" means most recent.
        let pinnedOld = record(id: "CCCCCCCC-0000-0000-0000-000000000005", lastOpened: 1_000, pinned: true)
        let recent = record(id: "CCCCCCCC-0000-0000-0000-000000000006", lastOpened: 2_000)
        let target = ThinkingMapModel.mostRecentAppendTarget(in: [pinnedOld, recent])
        XCTAssertEqual(target?.id, recent.id)
    }

    func testAppendTargetNilWhenLibraryEmptyOrAllArchived() {
        XCTAssertNil(ThinkingMapModel.mostRecentAppendTarget(in: []))
        let archived = record(id: "CCCCCCCC-0000-0000-0000-000000000007", lastOpened: 1_000, archived: true)
        XCTAssertNil(ThinkingMapModel.mostRecentAppendTarget(in: [archived]),
                     "all-archived library → nil → caller seeds a NEW map")
    }

    // MARK: - ThinkingNode.promotedKinds Codable back-compat

    func testThinkingNodeDecodesLegacySnapshotWithoutPromotedKinds() throws {
        // Old persisted E0 snapshots predate `promotedKinds` — a node encoded
        // WITHOUT the field must still decode (to []), keeping the read-only
        // importer path back-compatible.
        let (record, rootID, _) = sampleRecord()
        var legacy = try JSONSerialization.jsonObject(
            with: try JSONEncoder().encode(record.snapshot)) as! [String: Any]
        var nodes = legacy["nodes"] as! [[String: Any]]
        for i in nodes.indices { nodes[i].removeValue(forKey: "promotedKinds") }
        legacy["nodes"] = nodes
        let data = try JSONSerialization.data(withJSONObject: legacy)

        let decoded = try JSONDecoder().decode(ThinkingMapSnapshot.self, from: data)
        XCTAssertEqual(decoded.nodes.count, 2)
        XCTAssertEqual(decoded.nodes.first { $0.id == rootID }?.promotedKinds, [])
    }

    func testThinkingNodePromotedKindsRoundTrips() throws {
        let (record, rootID, _) = sampleRecord()
        var snapshot = record.snapshot
        snapshot.nodes[0].promotedKinds = ["task", "memory"]
        let decoded = try JSONDecoder().decode(
            ThinkingMapSnapshot.self, from: try JSONEncoder().encode(snapshot))
        XCTAssertEqual(decoded.nodes.first { $0.id == rootID }?.promotedKinds, ["task", "memory"])
    }
}
