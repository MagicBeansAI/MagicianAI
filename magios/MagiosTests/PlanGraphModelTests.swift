import CoreGraphics
import XCTest
@testable import Magician

/// The plan graph helpers behind the Plan Inspector — ports of the web
/// `plangraph.ts` (`isPlanAcyclic`) and `PlanGraphView.svelte`
/// (`calculateStepLevel`, `topoSortIds`, `getAvailableDeps`,
/// `wouldCreateCycle`, `addEditableStep`, `removeEditableStep`,
/// `makeStartingNode`, `rebuildEdges`, `requestSave`).
final class PlanGraphModelTests: XCTestCase {
    private func step(_ id: String, _ extra: [String: Any] = [:]) -> [String: Any] {
        var object: [String: Any] = ["id": id, "task": "Task \(id)", "tool": "tool_\(id)", "parameters": [:],
                                     "expected_outputs": [], "confidence": 0.8, "metadata": [:]]
        extra.forEach { object[$0.key] = $0.value }
        return object
    }

    private func graph(_ steps: [[String: Any]], _ edges: [(String, String, String)], extra: [String: Any] = [:]) -> PlanGraph {
        var object: [String: Any] = [
            "steps": steps,
            "edges": edges.map { ["from": $0.0, "to": $0.1, "reason": $0.2] },
            "unresolved_inputs": [], "confidence": 0.7,
            "provenance": ["strategy": "MCTS", "generator": NSNull(), "notes": NSNull()]
        ]
        extra.forEach { object[$0.key] = $0.value }
        return PlanGraph(json: PlanJSON(object))!
    }

    /// a → b, a → c, b → d, c → d; e alone. Steps listed out of order.
    private var diamond: PlanGraph {
        graph([step("d"), step("c"), step("b"), step("a"), step("e")],
              [("a", "b", "first"), ("a", "c", "second"), ("b", "d", "join b"), ("c", "d", "join c")])
    }

    // MARK: Decoding

    func testDecodesTolerantlyWhenFieldsAreMissing() {
        let decoded = PlanGraph(json: PlanJSON(["steps": [["task": "No id"], ["id": "x"]]]))!
        XCTAssertEqual(decoded.steps.map(\.id), ["step_1", "x"])
        XCTAssertEqual(decoded.steps[1].task, "")
        XCTAssertNil(decoded.steps[1].tool)
        XCTAssertEqual(decoded.steps[1].confidence, 0)
        XCTAssertTrue(decoded.edges.isEmpty)
        XCTAssertNil(decoded.confidence)
        XCTAssertNil(PlanGraph(json: PlanJSON("not an object")))
    }

    func testDependenciesComeFromEdgesAndFillInFromAStepsOwnDependsOn() {
        let decoded = graph([step("a"), step("b"), step("c", ["depends_on": ["a", "ghost"]])], [("b", "c", "edge")])
        XCTAssertEqual(decoded.step("c")?.dependsOn, ["b", "a"])
    }

    // MARK: Levels and order

    func testLevelIsOnePlusTheDeepestDependency() {
        XCTAssertEqual(diamond.levels, ["a": 0, "b": 1, "c": 1, "d": 2, "e": 0])
    }

    func testWaterfallPlacesEachStepOnceItsDependenciesArePlaced() {
        // Web topoSortIds: roots in plan order, children depth first.
        XCTAssertEqual(diamond.waterfallOrder, ["a", "c", "b", "d", "e"])
        XCTAssertEqual(diamond.waterfall.map(\.level), [0, 1, 1, 2, 0])
    }

    func testACycleStillListsEveryStepAndKeepsLevelsFinite() {
        let cyclic = graph([step("a"), step("b"), step("c")], [("a", "b", ""), ("b", "c", ""), ("c", "b", "")])
        XCTAssertFalse(cyclic.isAcyclic)
        XCTAssertEqual(Set(cyclic.waterfallOrder), ["a", "b", "c"])
        XCTAssertEqual(cyclic.waterfallOrder.count, 3)
        XCTAssertTrue(cyclic.levels.values.allSatisfy { $0 >= 0 && $0 < 10 })
        XCTAssertTrue(diamond.isAcyclic)
    }

    // MARK: Cycle-safe dependencies

    func testCandidatesExcludeSelfExistingDependenciesAndTransitiveDependents() {
        let chain = graph([step("a"), step("b"), step("c"), step("d")], [("a", "b", ""), ("b", "c", "")])
        XCTAssertEqual(chain.dependencyCandidates(for: "a").map(\.id), ["d"])
        XCTAssertEqual(chain.dependencyCandidates(for: "c").map(\.id), ["a", "d"])
        XCTAssertEqual(chain.dependencyCandidates(for: "missing").map(\.id), [])
    }

    func testEveryCandidateKeepsTheGraphAcyclic() {
        let base = diamond
        for target in base.steps {
            for candidate in base.dependencyCandidates(for: target.id) {
                var copy = base
                XCTAssertTrue(copy.setDependency(target.id, on: candidate.id, true))
                XCTAssertTrue(copy.isAcyclic, "\(target.id) → \(candidate.id) made a cycle")
            }
        }
    }

    func testSetDependencyRefusesACycleASelfLoopAndADuplicate() {
        var chain = graph([step("a"), step("b"), step("c")], [("a", "b", ""), ("b", "c", "")])
        XCTAssertTrue(chain.wouldCreateCycle(stepId: "a", dependingOn: "c"))
        XCTAssertFalse(chain.setDependency("a", on: "c", true))
        XCTAssertFalse(chain.setDependency("a", on: "a", true))
        XCTAssertFalse(chain.setDependency("b", on: "a", true))
        XCTAssertTrue(chain.setDependency("c", on: "a", true))
        XCTAssertEqual(chain.step("c")?.dependsOn, ["b", "a"])
        XCTAssertTrue(chain.setDependency("c", on: "b", false))
        XCTAssertEqual(chain.step("c")?.dependsOn, ["a"])
        XCTAssertTrue(chain.isAcyclic)
    }

    // MARK: Edit operations

    func testAddStepUsesEpochMillisAndTheWebDefaults() {
        var plan = diamond
        let now = Date(timeIntervalSince1970: 1_700_000_000.123)
        let id = plan.addStep(now: now)
        XCTAssertEqual(id, "step_1700000000123")
        let added = plan.step(id)!
        XCTAssertEqual(added.task, "New step")
        XCTAssertEqual(added.confidence, 0.5)
        XCTAssertNil(added.tool)
        XCTAssertTrue(added.isStart)
        XCTAssertEqual(plan.steps.last?.id, id)
        XCTAssertEqual(plan.addStep(now: now), "step_1700000000123_2")
    }

    func testRemoveStepDropsItAndEveryDependencyOnIt() {
        var plan = diamond
        plan.removeStep("b")
        XCTAssertNil(plan.step("b"))
        XCTAssertEqual(plan.step("d")?.dependsOn, ["c"])
        XCTAssertFalse(plan.saveEdges().contains { $0.from == "b" || $0.to == "b" })
        plan.removeSteps(atOffsets: IndexSet(integer: 0))
        XCTAssertNil(plan.step("d"))
    }

    func testMoveStepsReordersLikeArrayMove() {
        var plan = graph([step("a"), step("b"), step("c"), step("d")], [])
        plan.moveSteps(fromOffsets: IndexSet(integer: 0), toOffset: 3)
        XCTAssertEqual(plan.steps.map(\.id), ["b", "c", "a", "d"])
        plan.moveSteps(fromOffsets: IndexSet(integer: 3), toOffset: 0)
        XCTAssertEqual(plan.steps.map(\.id), ["d", "b", "c", "a"])
    }

    func testMakeStartClearsDependenciesAndSortByWaterfallReorders() {
        var plan = diamond
        plan.makeStart("d")
        XCTAssertTrue(plan.step("d")!.isStart)
        plan.sortByWaterfall()
        XCTAssertEqual(plan.steps.map(\.id), plan.waterfallOrder)
    }

    func testUpdateStepKeepsIdentity() {
        var plan = diamond
        plan.updateStep("a") { $0.id = "renamed"; $0.task = "Changed"; $0.confidence = 0.35 }
        XCTAssertEqual(plan.step("a")?.task, "Changed")
        XCTAssertEqual(plan.step("a")?.confidence, 0.35)
        XCTAssertNil(plan.step("renamed"))
    }

    // MARK: Save payload

    func testSaveRebuildsEdgesFromDependenciesKeepingOldReasons() {
        var plan = diamond
        XCTAssertTrue(plan.setDependency("e", on: "d", true))
        plan.removeStep("c")
        let edges = plan.saveEdges()
        XCTAssertEqual(edges, [
            PlanEdge(from: "b", to: "d", reason: "join b"),
            PlanEdge(from: "a", to: "b", reason: "first"),
            PlanEdge(from: "d", to: "e", reason: "dependency")
        ])
    }

    func testSavePreservesEveryFieldTheServerSent() {
        var plan = graph(
            [step("a", ["success_criteria": "done", "parameters": ["limit": 5, "flag": true, "name": "x"],
                        "providing_agent_id": "researcher", "readiness": "strong"]),
             step("b", ["depends_on": ["a"]])],
            [("a", "b", "needs a")],
            extra: ["unresolved_inputs": [["parameter": "who", "step_id": "b"]],
                    "session_policy": ["mode": "shared"], "planning_metadata": ["v": 2], "confidence": 0.61])
        plan.updateStep("a") { $0.task = "Edited"; $0.tool = "  " }
        let payload = plan.savePayload()

        XCTAssertEqual(payload["session_policy"], PlanJSON(["mode": "shared"]))
        XCTAssertEqual(payload["planning_metadata"], PlanJSON(["v": 2]))
        XCTAssertEqual(payload["unresolved_inputs"]?.arrayValue?.count, 1)
        XCTAssertEqual(payload["confidence"], .number(0.61))
        XCTAssertEqual(payload["provenance"]?["strategy"], .string("MCTS"))

        let steps = payload["steps"]!.arrayValue!
        let a = steps[0]
        XCTAssertEqual(a["task"], .string("Edited"))
        XCTAssertEqual(a["tool"], .null)
        XCTAssertEqual(a["success_criteria"], .string("done"))
        XCTAssertEqual(a["readiness"], .string("strong"))
        XCTAssertEqual(a["providing_agent_id"], .string("researcher"))
        XCTAssertEqual(a["parameters"]?["limit"], .number(5))
        XCTAssertEqual(a["parameters"]?["flag"], .bool(true))
        XCTAssertNil(a["depends_on"])
        XCTAssertEqual(steps[1]["depends_on"], PlanJSON(["a"]))
        XCTAssertEqual(payload["edges"], PlanJSON([["from": "a", "to": "b", "reason": "needs a"]]))

        // Survives a JSON round trip with types intact.
        let data = try! JSONSerialization.data(withJSONObject: PlanJSON.object(payload).foundation)
        let reparsed = PlanGraph(json: PlanJSON.parse(data: data))!
        XCTAssertEqual(reparsed.step("a")?.parameters.first { $0.key == "limit" }?.value, .number(5))
        XCTAssertEqual(reparsed.step("b")?.dependsOn, ["a"])
    }

    func testANewStepSavesTheFieldsTheServerRequires() {
        var plan = PlanGraph.empty
        let id = plan.addStep(now: Date(timeIntervalSince1970: 1))
        let saved = plan.savePayload()["steps"]!.arrayValue![0]
        XCTAssertEqual(saved["id"], .string(id))
        XCTAssertEqual(saved["expected_outputs"], .array([]))
        XCTAssertEqual(saved["metadata"], .object([:]))
        XCTAssertEqual(saved["parameters"], .object([:]))
        XCTAssertEqual(saved["confidence"], .number(0.5))
        XCTAssertNotNil(plan.savePayload()["provenance"]?["strategy"])
    }

    func testParameterEditsKeepANonStringTypeWhenTheTextIsJSON() {
        XCTAssertEqual(PlanParameter.value(fromText: "12", previous: .number(5)), .number(12))
        XCTAssertEqual(PlanParameter.value(fromText: "1x", previous: .number(5)), .string("1x"))
        XCTAssertEqual(PlanParameter.value(fromText: "12", previous: .string("a")), .string("12"))
        XCTAssertEqual(PlanParameter.value(fromText: "hi", previous: nil), .string("hi"))
    }

    // MARK: Layout

    func testLayoutPutsEachLevelOnItsOwnRow() {
        let layout = PlanGraphLayout(graph: diamond)
        XCTAssertEqual(layout.layers.count, 3)
        let pitch = layout.metrics.nodeSize.height + layout.metrics.verticalGap
        for (index, layer) in layout.layers.enumerated() {
            for id in layer { XCTAssertEqual(layout.positions[id]!.y, layout.positions["a"]!.y + CGFloat(index) * pitch) }
        }
        XCTAssertEqual(Set(layout.layers[0]), ["a", "e"])
        XCTAssertEqual(layout.layers[2], ["d"])
        XCTAssertGreaterThan(layout.size.width, layout.metrics.nodeSize.width * 2)
    }

    func testChildrenAreOrderedByTheirParentsMeanPosition() {
        // Roots p, q, r left to right; x hangs off r, y off p, z off p and q.
        let plan = graph([step("p"), step("q"), step("r"), step("x"), step("y"), step("z")],
                         [("r", "x", ""), ("p", "y", ""), ("p", "z", ""), ("q", "z", "")])
        let layout = PlanGraphLayout(graph: plan)
        XCTAssertEqual(layout.layers[0], ["p", "q", "r"])
        XCTAssertEqual(layout.layers[1], ["y", "z", "x"])
        let xs = layout.layers[1].map { layout.positions[$0]!.x }
        XCTAssertEqual(xs, xs.sorted())
    }

    func testFitScaleIsClampedAndNeverEnlarges() {
        let layout = PlanGraphLayout(graph: diamond)
        XCTAssertEqual(layout.fitScale(in: CGSize(width: 10_000, height: 10_000)), 1)
        XCTAssertEqual(layout.fitScale(in: CGSize(width: 10, height: 10)), 0.3)
        XCTAssertEqual(PlanGraphLayout.clampScale(9), 3)
        XCTAssertEqual(PlanGraphLayout(graph: .empty).size, .zero)
    }

    func testSegmentsRunFromParentBottomToChildTop() {
        let layout = PlanGraphLayout(graph: diamond)
        let segments = layout.segments(for: diamond)
        XCTAssertEqual(segments.count, 4)
        for segment in segments { XCTAssertLessThan(segment.from.y, segment.to.y) }
    }

    func testConfidenceBands() {
        XCTAssertEqual(PlanConfidenceBand(0.7), .high)
        XCTAssertEqual(PlanConfidenceBand(0.69), .medium)
        XCTAssertEqual(PlanConfidenceBand(0.4), .medium)
        XCTAssertEqual(PlanConfidenceBand(0.39), .low)
    }
}
