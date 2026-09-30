//  CanonicalThinkingMapBackendTests.swift
//  Verifies the PURE, UNGATED halves of the canonical Thinking Map backend:
//    - `CanonicalThinkingMapProjection.project` (canonical LTM.Map → E0 view)
//    - `CanonicalThinkingMapOps` op-builders (E0 mutation → [LTM.Operation])
//
//  These are compiled + run in the normal (flag-OFF) build, so they guard the
//  read/write translation without needing a live backend or the gated runtime.

import XCTest
@testable import Magician

final class CanonicalThinkingMapBackendTests: XCTestCase {

    // MARK: - Fixtures

    /// A hand-built canonical map exercising every projection rule:
    ///   node-1  owner_spoken/asserted decision  (root)
    ///   node-2  model_inferred/provisional risk (child of node-1) → suggested
    ///   node-3  owner_spoken/asserted idea       (child of node-1)
    ///   node-4  tombstoned                        → dropped
    ///   edge-r  related_to node-1 ↔ node-3        → .related
    ///   edge-t  related_to (tombstoned)           → dropped
    ///   view active_node = node-2
    private func sampleMap() -> LTM.Map {
        func node(
            _ id: String, kind: LTM.NodeKind, label: String,
            origin: LTM.AssertionOrigin, state: LTM.EpistemicState,
            parent: String? = nil, detail: String? = nil, tombstoned: Bool = false,
            created: String
        ) -> LTM.Node {
            LTM.Node(
                nodeId: id, kind: kind, label: label, detailMarkdown: detail,
                epistemicState: state, assertionOrigin: origin, confidence: 1.0,
                parentId: parent, tombstoned: tombstoned,
                createdAt: created, updatedAt: created)
        }
        func edge(
            _ id: String, from: String, to: String, kind: LTM.EdgeKind,
            tombstoned: Bool = false
        ) -> LTM.Edge {
            LTM.Edge(
                edgeId: id, fromNode: from, toNode: to, kind: kind,
                assertionOrigin: .ownerSpoken, tombstoned: tombstoned,
                createdAt: "2026-07-20T00:00:00Z", updatedAt: "2026-07-20T00:00:00Z")
        }
        let nodes = [
            node("node-1", kind: .decision, label: "Ship the map",
                 origin: .ownerSpoken, state: .asserted, detail: "Decide to ship.",
                 created: "2026-07-20T00:00:01Z"),
            node("node-2", kind: .risk, label: "Contract drift",
                 origin: .modelInferred, state: .provisional, parent: "node-1",
                 created: "2026-07-20T00:00:02Z"),
            node("node-3", kind: .idea, label: "Focus mode",
                 origin: .ownerSpoken, state: .asserted, parent: "node-1",
                 created: "2026-07-20T00:00:03Z"),
            node("node-4", kind: .action, label: "Dead branch",
                 origin: .ownerSpoken, state: .asserted, parent: "node-1",
                 tombstoned: true, created: "2026-07-20T00:00:04Z"),
        ]
        let edges = [
            edge("edge-r", from: "node-1", to: "node-3", kind: .relatedTo),
            edge("edge-t", from: "node-1", to: "node-2", kind: .relatedTo, tombstoned: true),
        ]
        return LTM.Map(
            schemaVersion: 1, mapId: "map-1", principal: "anonymous",
            workspace: "default", title: "Sample", source: .solo, revision: 1,
            viewState: LTM.SharedViewState(activeNode: "node-2", lens: .graph),
            nodes: Dictionary(uniqueKeysWithValues: nodes.map { ($0.nodeId, $0) }),
            edges: Dictionary(uniqueKeysWithValues: edges.map { ($0.edgeId, $0) }),
            createdAt: "2026-07-20T00:00:00Z", updatedAt: "2026-07-20T01:00:00Z")
    }

    // MARK: - Projection

    func testProjectMapsNodesKindsTitlesAndSuggested() throws {
        let projected = CanonicalThinkingMapProjection.project(sampleMap())

        // node-4 (tombstoned) is dropped → 3 live nodes.
        XCTAssertEqual(projected.nodes.count, 3)

        let byTitle = Dictionary(uniqueKeysWithValues: projected.nodes.map { ($0.title, $0) })

        let root = try XCTUnwrap(byTitle["Ship the map"])
        XCTAssertEqual(root.kind, .decision)
        XCTAssertEqual(root.detail, "Decide to ship.")
        XCTAssertEqual(root.suggested, false)          // owner_spoken + asserted
        XCTAssertNil(root.parentID)

        let risk = byTitle["Contract drift"]
        XCTAssertEqual(risk?.kind, .risk)
        XCTAssertEqual(risk?.suggested, true)         // model_inferred + provisional
        XCTAssertNotNil(risk?.parentID)

        let idea = byTitle["Focus mode"]
        XCTAssertEqual(idea?.kind, .idea)
        XCTAssertEqual(idea?.suggested, false)
    }

    func testProjectSynthesizesBranchEdgeForChildren() {
        let projected = CanonicalThinkingMapProjection.project(sampleMap())

        let rootID = CanonicalThinkingMapProjection.deterministicUUID(from: "node-1")
        let riskID = CanonicalThinkingMapProjection.deterministicUUID(from: "node-2")
        let ideaID = CanonicalThinkingMapProjection.deterministicUUID(from: "node-3")

        let branchEdges = projected.edges.filter { $0.kind == .branch }
        // Two live children (node-2, node-3) → two synthetic branch edges; the
        // tombstoned node-4 contributes none.
        XCTAssertEqual(branchEdges.count, 2)
        XCTAssertTrue(branchEdges.contains { $0.from == rootID && $0.to == riskID })
        XCTAssertTrue(branchEdges.contains { $0.from == rootID && $0.to == ideaID })
    }

    func testProjectMapsRelatedEdgeAndDropsTombstoned() {
        let projected = CanonicalThinkingMapProjection.project(sampleMap())

        let relatedEdges = projected.edges.filter { $0.kind == .related }
        // Only edge-r survives; edge-t is tombstoned → dropped.
        XCTAssertEqual(relatedEdges.count, 1)

        let rootID = CanonicalThinkingMapProjection.deterministicUUID(from: "node-1")
        let ideaID = CanonicalThinkingMapProjection.deterministicUUID(from: "node-3")
        let edge = relatedEdges[0]
        XCTAssertEqual(Set([edge.from, edge.to]), Set([rootID, ideaID]))
    }

    func testProjectMapsActiveNode() {
        let projected = CanonicalThinkingMapProjection.project(sampleMap())
        let riskID = CanonicalThinkingMapProjection.deterministicUUID(from: "node-2")
        XCTAssertEqual(projected.activeNodeID, riskID)
    }

    func testProjectRetainsExactCanonicalNodeAndEdgeIdentifiers() throws {
        let projected = CanonicalThinkingMapProjection.project(sampleMap())
        let nodeID = CanonicalThinkingMapProjection.deterministicUUID(from: "node-2")
        let edge = try XCTUnwrap(projected.edges.first { $0.kind == .related })

        XCTAssertEqual(projected.canonicalNodeIDs[nodeID], "node-2")
        XCTAssertEqual(projected.canonicalEdgeIDs[edge.id], "edge-r")
    }

    func testProjectDoesNotChangeCanonicalUUIDLetterCase() {
        let lowerID = "12620746-a25c-4e29-80ca-e112c8d32aa7"
        var map = sampleMap()
        map.nodes[lowerID] = LTM.Node(
            nodeId: lowerID,
            kind: .question,
            label: "What makes it sentimental?",
            detailMarkdown: nil,
            epistemicState: .asserted,
            assertionOrigin: .ownerSpoken,
            confidence: 1,
            createdAt: "2026-07-20T00:00:05Z",
            updatedAt: "2026-07-20T00:00:05Z")

        let projected = CanonicalThinkingMapProjection.project(map)
        let viewID = UUID(uuidString: lowerID)!
        XCTAssertEqual(projected.canonicalNodeIDs[viewID], lowerID)
        XCTAssertNotEqual(projected.canonicalNodeIDs[viewID], viewID.uuidString)
    }

    func testPendingLocalSelectionOutranksStaleRootUntilAcknowledged() {
        let root = UUID()
        let selected = UUID()
        let live = Set([root, selected])

        let stale = ThinkingMapModel.reconcileActiveNodeSelection(
            projected: root, pending: selected, liveNodeIDs: live)
        XCTAssertEqual(stale.active, selected)
        XCTAssertEqual(stale.pending, selected)

        let acknowledged = ThinkingMapModel.reconcileActiveNodeSelection(
            projected: selected, pending: selected, liveNodeIDs: live)
        XCTAssertEqual(acknowledged.active, selected)
        XCTAssertNil(acknowledged.pending)

        let removed = ThinkingMapModel.reconcileActiveNodeSelection(
            projected: root, pending: selected, liveNodeIDs: [root])
        XCTAssertEqual(removed.active, root)
        XCTAssertNil(removed.pending)
    }

    // MARK: - Clarification / restructure projection

    /// A map with an OPEN and an ANSWERED clarification → only the open one is
    /// projected, carrying its question + the node UUID the node projection uses.
    func testProjectClarificationsKeepsOnlyOpen() {
        func clar(
            _ id: String, node: String, question: String,
            state: LTM.ClarificationState, answer: String? = nil, created: String
        ) -> LTM.Clarification {
            LTM.Clarification(
                clarificationId: id, nodeId: node, question: question,
                state: state, answer: answer, createdAt: created)
        }
        let clars = [
            clar("c-open", node: "node-1", question: "Which platform first?",
                 state: .open, created: "2026-07-20T00:00:01Z"),
            clar("c-done", node: "node-3", question: "Already resolved?",
                 state: .answered, answer: "yes", created: "2026-07-20T00:00:02Z"),
        ]
        var map = sampleMap()
        map.clarifications = Dictionary(uniqueKeysWithValues: clars.map { ($0.clarificationId, $0) })

        let projected = CanonicalThinkingMapProjection.projectClarifications(map)
        XCTAssertEqual(projected.count, 1)
        let only = projected[0]
        XCTAssertEqual(only.id, "c-open")
        XCTAssertEqual(only.question, "Which platform first?")
        XCTAssertFalse(only.answered)
        XCTAssertNil(only.answer)
        // nodeID lines up with the SAME UUID the node projection assigns node-1.
        XCTAssertEqual(only.nodeID, CanonicalThinkingMapProjection.deterministicUUID(from: "node-1"))
    }

    /// Clarifications sort by createdAt then id (deterministic).
    func testProjectClarificationsDeterministicOrder() {
        func openClar(_ id: String, created: String) -> LTM.Clarification {
            LTM.Clarification(
                clarificationId: id, nodeId: "node-1", question: "q-\(id)",
                state: .open, createdAt: created)
        }
        var map = sampleMap()
        let clars = [
            openClar("c-b", created: "2026-07-20T00:00:02Z"),
            openClar("c-a", created: "2026-07-20T00:00:01Z"),
            openClar("c-c", created: "2026-07-20T00:00:02Z"),
        ]
        map.clarifications = Dictionary(uniqueKeysWithValues: clars.map { ($0.clarificationId, $0) })

        let ids = CanonicalThinkingMapProjection.projectClarifications(map).map(\.id)
        XCTAssertEqual(ids, ["c-a", "c-b", "c-c"]) // c-a earliest; c-b<c-c by id tiebreak
    }

    /// A map with a `proposed` and a `confirmed` proposal → only the proposed one,
    /// with operationCount + affected node UUIDs matching the node projection.
    func testProjectProposalsKeepsOnlyProposed() {
        let owner = LTM.Actor.owner(principal: "anonymous")
        // Two staged ops on the proposed proposal → operationCount == 2.
        let stagedOps: [LTM.Operation] = [
            .tombstoneNode(nodeId: "node-2"),
            .moveToParent(nodeId: "node-3", parentId: nil),
        ]
        let proposals = [
            LTM.RestructureProposal(
                proposalId: "p-open", proposedBy: owner, rationale: "Group by theme",
                operations: stagedOps, state: .proposed,
                affectedNodeIds: ["node-1", "node-3"],
                createdAt: "2026-07-20T00:00:01Z"),
            LTM.RestructureProposal(
                proposalId: "p-done", proposedBy: owner, rationale: "Already applied",
                operations: [.tombstoneNode(nodeId: "node-1")], state: .confirmed,
                affectedNodeIds: ["node-1"], createdAt: "2026-07-20T00:00:02Z"),
        ]
        var map = sampleMap()
        map.proposals = Dictionary(uniqueKeysWithValues: proposals.map { ($0.proposalId, $0) })

        let projected = CanonicalThinkingMapProjection.projectProposals(map)
        XCTAssertEqual(projected.count, 1)
        let only = projected[0]
        XCTAssertEqual(only.id, "p-open")
        XCTAssertEqual(only.rationale, "Group by theme")
        XCTAssertEqual(only.operationCount, 2)
        XCTAssertEqual(
            only.affectedNodeIDs,
            ["node-1", "node-3"].map(CanonicalThinkingMapProjection.deterministicUUID(from:)))
    }

    func testProjectDecodesWireFixture() throws {
        // The canonical wire fixture decodes and projects cleanly (belt + braces
        // against the manual Codable + the projection agreeing).
        let data = Data(LTMWireFixtures.mapJSON.utf8)
        let map = try LTM.Wire.makeDecoder().decode(LTM.Map.self, from: data)
        let projected = CanonicalThinkingMapProjection.project(map)

        // node-1 (owner/asserted) + node-2 (model/provisional, child of node-1).
        XCTAssertEqual(projected.nodes.count, 2)
        XCTAssertTrue(projected.nodes.contains { $0.title == "Ship the iOS thinking map" && !$0.suggested })
        XCTAssertTrue(projected.nodes.contains { $0.kind == .risk && $0.suggested })
        // A synthetic branch edge (node-1 → node-2) and the related_to edge-1.
        XCTAssertEqual(projected.edges.filter { $0.kind == .branch }.count, 1)
        XCTAssertEqual(projected.edges.filter { $0.kind == .related }.count, 1)
    }

    func testDeterministicUUIDIsStable() {
        let a = CanonicalThinkingMapProjection.deterministicUUID(from: "node-x")
        let b = CanonicalThinkingMapProjection.deterministicUUID(from: "node-x")
        let c = CanonicalThinkingMapProjection.deterministicUUID(from: "node-y")
        XCTAssertEqual(a, b)
        XCTAssertNotEqual(a, c)
    }

    // MARK: - Op builders

    func testAddThoughtYieldsAddNodeOwnerSpokenAndMoveToParent() {
        let built = CanonicalThinkingMapOps.addThought(
            text: "A new idea", kind: .question, activeID: "parent-1")

        XCTAssertEqual(built.ops.count, 2)

        guard case let .addNode(node) = built.ops[0] else {
            return XCTFail("first op should be add_node")
        }
        XCTAssertEqual(node.nodeId, built.newNodeID)
        XCTAssertEqual(node.label, "A new idea")
        XCTAssertEqual(node.kind, .question)
        XCTAssertEqual(node.assertionOrigin, .ownerSpoken)
        XCTAssertEqual(node.epistemicState, .asserted)
        XCTAssertEqual(node.parentId, "parent-1")

        guard case let .moveToParent(nodeId, parentId) = built.ops[1] else {
            return XCTFail("second op should be move_to_parent")
        }
        XCTAssertEqual(nodeId, built.newNodeID)
        XCTAssertEqual(parentId, "parent-1")
    }

    func testAddThoughtWithNoActiveOmitsMoveToParent() {
        let built = CanonicalThinkingMapOps.addThought(
            text: "Root", kind: .idea, activeID: nil)
        XCTAssertEqual(built.ops.count, 1)
        guard case let .addNode(node) = built.ops[0] else {
            return XCTFail("only op should be add_node")
        }
        XCTAssertNil(node.parentId)
    }

    func testAddThoughtDetailOverrideCarriesProvenanceKeepsOwnerOrigin() {
        // Share ingestion: the label is the bounded thought, the detail carries
        // the full text behind a provenance line — while the origin STAYS
        // owner_spoken (the owner /operations surface rejects imported_source).
        let detail = "Shared from example.com — https://example.com/a\n\nFull article body"
        let built = CanonicalThinkingMapOps.addThought(
            text: "Full article body", kind: .idea, activeID: nil, detail: detail)
        guard case let .addNode(node) = built.ops[0] else {
            return XCTFail("first op should be add_node")
        }
        XCTAssertEqual(node.label, "Full article body")
        XCTAssertEqual(node.detailMarkdown, detail)
        XCTAssertEqual(node.assertionOrigin, .ownerSpoken)
        XCTAssertEqual(node.epistemicState, .asserted)
    }

    func testAddThoughtNilDetailDefaultsToText() {
        let built = CanonicalThinkingMapOps.addThought(
            text: "Plain capture", kind: .idea, activeID: nil)
        guard case let .addNode(node) = built.ops[0] else {
            return XCTFail("first op should be add_node")
        }
        XCTAssertEqual(node.detailMarkdown, "Plain capture")
    }

    // MARK: - Share seeding (ShareThinkingMapSeed)

    func testShareSeedTextWithSourceURLPrefixesHostProvenance() throws {
        let seed = try XCTUnwrap(ShareThinkingMapSeed.build(
            text: "A shared insight", sourceURL: "https://example.com/post"))
        XCTAssertEqual(seed.thought, "A shared insight")
        XCTAssertEqual(
            seed.detail,
            "Shared from example.com — https://example.com/post\n\nA shared insight")
    }

    func testShareSeedTextOnlyUsesGenericProvenance() throws {
        let seed = try XCTUnwrap(ShareThinkingMapSeed.build(
            text: "  Just text  ", sourceURL: nil))
        XCTAssertEqual(seed.thought, "Just text")
        XCTAssertEqual(seed.detail, "Shared from another app\n\nJust text")
    }

    func testShareSeedURLOnlyShareSeedsTheLink() throws {
        // A webpage share enqueues the URL string as the text.
        let url = "https://news.example.org/story"
        let seed = try XCTUnwrap(ShareThinkingMapSeed.build(text: url, sourceURL: url))
        XCTAssertEqual(seed.thought, url)
        XCTAssertEqual(seed.detail, "Shared from news.example.org — \(url)\n\n\(url)")
    }

    func testShareSeedEmptyReturnsNil() {
        XCTAssertNil(ShareThinkingMapSeed.build(text: "   \n  ", sourceURL: nil))
        XCTAssertNil(ShareThinkingMapSeed.build(text: "", sourceURL: "   "))
    }

    func testShareSeedLongTextBoundsLabelAndKeepsFullBodyInDetail() throws {
        let body = String(repeating: "word ", count: 100).trimmingCharacters(in: .whitespaces)
        let seed = try XCTUnwrap(ShareThinkingMapSeed.build(text: body, sourceURL: nil))
        XCTAssertEqual(seed.thought.count, ShareThinkingMapSeed.maxThoughtLength)
        XCTAssertTrue(seed.thought.hasSuffix("…"))
        XCTAssertTrue(seed.detail.hasSuffix(body), "the FULL text must survive in the detail")
    }

    func testShareSeedMultilineUsesFirstNonEmptyLineAsLabel() throws {
        let seed = try XCTUnwrap(ShareThinkingMapSeed.build(
            text: "\n\nHeadline line\nSecond paragraph of the share.",
            sourceURL: nil))
        XCTAssertEqual(seed.thought, "Headline line")
        XCTAssertTrue(seed.detail.contains("Second paragraph of the share."))
    }

    func testUpdateNodeOnProvisionalAddsSetEpistemicState() {
        let ops = CanonicalThinkingMapOps.updateNode(
            id: "node-2", title: "Edited", detail: "body", wasProvisional: true)

        XCTAssertEqual(ops.count, 2)
        guard case let .updateNode(nodeId, label, detail, confidence) = ops[0] else {
            return XCTFail("first op should be update_node")
        }
        XCTAssertEqual(nodeId, "node-2")
        XCTAssertEqual(label, "Edited")
        XCTAssertEqual(detail, .set("body"))
        XCTAssertNil(confidence)

        guard case let .setEpistemicState(promotedID, state) = ops[1] else {
            return XCTFail("second op should be set_epistemic_state")
        }
        XCTAssertEqual(promotedID, "node-2")
        XCTAssertEqual(state, .asserted)      // editing promotes model → owner-asserted
    }

    func testUpdateNodeOnAssertedNodeOmitsPromotion() {
        let ops = CanonicalThinkingMapOps.updateNode(
            id: "node-1", title: "Edited", detail: "", wasProvisional: false)
        XCTAssertEqual(ops.count, 1)
        guard case let .updateNode(_, _, detail, _) = ops[0] else {
            return XCTFail("only op should be update_node")
        }
        XCTAssertEqual(detail, .clear)        // empty detail clears the field
    }

    func testConnectYieldsRelatedToEdge() {
        let ops = CanonicalThinkingMapOps.connect(from: "node-1", to: "node-2")
        XCTAssertEqual(ops.count, 1)
        guard case let .connect(edge) = ops[0] else {
            return XCTFail("op should be connect")
        }
        XCTAssertEqual(edge.fromNode, "node-1")
        XCTAssertEqual(edge.toNode, "node-2")
        XCTAssertEqual(edge.kind, .relatedTo)
        XCTAssertEqual(edge.assertionOrigin, .ownerSpoken)
        XCTAssertFalse(edge.edgeId.isEmpty)
    }

    func testDisconnectYieldsDisconnectByEdgeID() {
        let ops = CanonicalThinkingMapOps.disconnect(edgeID: "edge-9")
        XCTAssertEqual(ops.count, 1)
        guard case let .disconnect(edgeId) = ops[0] else {
            return XCTFail("op should be disconnect")
        }
        XCTAssertEqual(edgeId, "edge-9")
    }

    func testRemoveBranchYieldsTombstoneNode() {
        let ops = CanonicalThinkingMapOps.removeBranch(id: "node-7")
        XCTAssertEqual(ops.count, 1)
        guard case let .tombstoneNode(nodeId) = ops[0] else {
            return XCTFail("op should be tombstone_node")
        }
        XCTAssertEqual(nodeId, "node-7")
    }

    func testSetTitleYieldsSetTitle() {
        let ops = CanonicalThinkingMapOps.setTitle("Renamed")
        XCTAssertEqual(ops.count, 1)
        guard case let .setTitle(title) = ops[0] else {
            return XCTFail("op should be set_title")
        }
        XCTAssertEqual(title, "Renamed")
    }

    func testCanonicalKindMapsAllElevenKinds() {
        // Every E0 kind maps to a canonical kind of the same snake_case name.
        for kind in ThinkingNodeKind.allCases {
            let canonical = CanonicalThinkingMapOps.canonicalKind(kind)
            XCTAssertEqual(canonical.rawValue, kind.rawValue.lowercased())
        }
    }

    // MARK: - AI frontier intent mapping (S2d)

    /// The E0 AI-bridge intent maps 1:1 onto the canonical `/interpret` steering
    /// intent. This is the pure piece of the S2d canonical frontier path; it is
    /// ungated so it compiles + runs in the normal (flag-OFF) build.
    func testFrontierIntentMapsToInterpretIntent() {
        XCTAssertEqual(ThinkingMapFrontierIntent.continueThinking.interpretIntent, .continueThinking)
        XCTAssertEqual(ThinkingMapFrontierIntent.breakOpen.interpretIntent, .breakOpen)
    }
}
