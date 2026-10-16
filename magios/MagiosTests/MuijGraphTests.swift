import XCTest
@testable import Magician

/// Graph family (plan 1.5): the bounded MUIJ graph model behind the native
/// iOS renderer — parsing, hostile-input degradation and deterministic tiers.
final class MuijGraphTests: XCTestCase {
    private func node(_ id: String, label: String? = nil, kind: String? = nil,
                      metadata: [String: JSONValue]? = nil) -> JSONValue {
        var object: [String: JSONValue] = ["id": .string(id), "label": .string(label ?? id)]
        if let kind { object["kind"] = .string(kind) }
        if let metadata { object["metadata"] = .object(metadata) }
        return .object(object)
    }

    private func edge(_ from: String, _ to: String, label: String? = nil) -> JSONValue {
        var object: [String: JSONValue] = ["from": .string(from), "to": .string(to)]
        if let label { object["label"] = .string(label) }
        return .object(object)
    }

    private func parseGraph(nodes: [JSONValue], edges: [JSONValue],
                            layout: JSONValue? = nil, focus: JSONValue? = nil,
                            reveal: JSONValue? = nil) throws -> MuijGraphModel {
        var props: [String: JSONValue] = ["nodes": .array(nodes), "edges": .array(edges)]
        if let layout { props["layout"] = layout }
        if let focus { props["focus_node_id"] = focus }
        if let reveal { props["reveal_order"] = reveal }
        let raw: JSONValue = .object([
            "muij_version": .string("1.0"),
            "agent_id": .string("surface"),
            "layout": .array([.object([
                "id": .string("graph"), "component_type": .string("Graph"),
                "label": .string("Pipeline"), "props": .object(props),
            ])]),
        ])
        let document = try XCTUnwrap(try? MuijDocumentModel.parse(raw).get())
        return try XCTUnwrap(document.layout.first?.graphModel)
    }

    func testGraphModelParsesNodesEdgesLayoutFocusAndReveal() throws {
        let model = try parseGraph(
            nodes: [
                node("root", label: "Root", kind: "core", metadata: [
                    "status": .string("active"), "attempts": .number(2)
                ]),
                node("leaf", label: "Leaf")
            ],
            edges: [edge("root", "leaf", label: "drives")],
            layout: .string("radial"),
            focus: .string("root"),
            reveal: .array([.string("root"), .string("leaf")])
        )

        XCTAssertEqual(model.nodes.map(\.id), ["root", "leaf"])
        XCTAssertEqual(model.nodes.first?.kind, "core")
        XCTAssertEqual(model.nodes.first?.metadata.map(\.key), ["attempts", "status"])
        XCTAssertEqual(model.nodes.first?.metadata.map(\.value), ["2", "active"])
        XCTAssertEqual(model.edges, [.init(from: "root", to: "leaf", label: "drives")])
        XCTAssertEqual(model.layout, .radial)
        XCTAssertEqual(model.focusNodeID, "root")
        XCTAssertEqual(model.revealOrder, ["root", "leaf"])
    }

    func testGraphTiersAreDeterministicAndCyclesShareAFinalTier() throws {
        let diamond = try parseGraph(
            nodes: [node("a"), node("b"), node("c"), node("d")],
            edges: [edge("a", "b"), edge("a", "c"), edge("b", "d"), edge("c", "d")]
        )
        let tier = { (id: String) in diamond.nodes.first(where: { $0.id == id })?.tier }
        XCTAssertEqual(tier("a"), 0)
        XCTAssertEqual(tier("b"), 1)
        XCTAssertEqual(tier("c"), 1)
        XCTAssertEqual(tier("d"), 2)

        let cyclic = try parseGraph(
            nodes: [node("entry"), node("loopA"), node("loopB")],
            edges: [edge("entry", "loopA"), edge("loopA", "loopB"), edge("loopB", "loopA")]
        )
        let cycleTier = { (id: String) in cyclic.nodes.first(where: { $0.id == id })?.tier }
        XCTAssertEqual(cycleTier("entry"), 0)
        XCTAssertEqual(cycleTier("loopA"), 1)
        XCTAssertEqual(cycleTier("loopB"), 1)
    }

    func testGraphSkipsMalformedMembersAndDanglingEdges() throws {
        let model = try parseGraph(
            nodes: [.null, node("keep"), node("keep"), .string("not-an-object"),
                    .object(["label": .string("missing id")])],
            edges: [edge("keep", "ghost"), .object(["from": .string("keep")]), edge("keep", "keep")]
        )

        XCTAssertEqual(model.nodes.map(\.id), ["keep"])
        XCTAssertEqual(model.edges, [.init(from: "keep", to: "keep", label: nil)])
    }

    func testGraphAdmissionIsCappedAtTheBoundedSizes() throws {
        let manyNodes = (0..<(MuijComponentModel.graphMaximumNodes + 5))
            .map { node("n\($0)") }
        let manyEdges = (0..<(MuijComponentModel.graphMaximumEdges + 5))
            .map { _ in edge("n0", "n1") }
        let model = try parseGraph(nodes: manyNodes, edges: manyEdges)

        XCTAssertEqual(model.nodes.count, MuijComponentModel.graphMaximumNodes)
        XCTAssertEqual(model.edges.count, MuijComponentModel.graphMaximumEdges)
    }

    func testUnknownGraphLayoutFallsBackToLayeredAndFocusMustBeDeclared() throws {
        let fallback = try parseGraph(nodes: [node("only")], edges: [], layout: .string("physics"))
        XCTAssertEqual(fallback.layout, .layered)

        let list = try parseGraph(nodes: [node("only")], edges: [], layout: .string("list"))
        XCTAssertEqual(list.layout, .list)

        let unfocused = try parseGraph(nodes: [node("only")], edges: [], focus: .string("ghost"))
        XCTAssertNil(unfocused.focusNodeID)
    }

    func testGraphMetadataIsBoundedToEightSortedEntries() throws {
        var metadata: [String: JSONValue] = [:]
        for index in 0..<12 { metadata["k\(index)"] = .number(Double(index)) }
        let model = try parseGraph(nodes: [node("meta", metadata: metadata)], edges: [])

        XCTAssertEqual(model.nodes.first?.metadata.count, MuijComponentModel.graphMaximumMetadataEntries)
        XCTAssertEqual(model.nodes.first?.metadata.map(\.key).first, "k0")
    }

    func testGraphIdsCompareExactlyWithoutTrimming() throws {
        // Ids compare exactly like the Rust validator (no trim): a node id
        // with surrounding spaces and an edge naming that same untrimmed id
        // is a Rust-valid document and must stay connected.
        let model = try parseGraph(
            nodes: [node(" a"), node("b")],
            edges: [edge(" a", "b")]
        )

        XCTAssertEqual(model.nodes.map(\.id), [" a", "b"])
        XCTAssertEqual(model.edges, [.init(from: " a", to: "b", label: nil)])
    }

    func testGraphDropsNodesWhoseIdsExceedTheIdCap() throws {
        let overlongId = String(repeating: "a", count: MuijComponentModel.graphMaximumNodeIdCharacters + 1)
        let model = try parseGraph(
            nodes: [node(overlongId), node("keep")],
            edges: [edge(overlongId, "keep")]
        )

        XCTAssertEqual(model.nodes.map(\.id), ["keep"])
        XCTAssertTrue(model.edges.isEmpty)
    }
}
