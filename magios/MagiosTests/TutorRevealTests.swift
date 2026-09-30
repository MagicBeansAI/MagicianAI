import XCTest
@testable import Magician

final class TutorRevealTests: XCTestCase {
    func testFittedRectLetterboxesAndProjectsCornersAndMidpoint() {
        let fitted = tutorFittedRect(imageSize: CGSize(width: 100, height: 200), in: CGSize(width: 300, height: 300))
        XCTAssertEqual(fitted, CGRect(x: 75, y: 0, width: 150, height: 300))
        let space = TutorShape.CoordinateSpace(width: 100, height: 200)
        XCTAssertEqual(tutorProject(point: .zero, coordinateSpace: space, fittedRect: fitted), CGPoint(x: 75, y: 0))
        XCTAssertEqual(tutorProject(point: CGPoint(x: 100, y: 200), coordinateSpace: space, fittedRect: fitted), CGPoint(x: 225, y: 300))
        XCTAssertEqual(tutorProject(point: CGPoint(x: 50, y: 100), coordinateSpace: space, fittedRect: fitted), CGPoint(x: 150, y: 150))
    }

    func testProjectionClampsOutOfBoundsCoordinates() {
        let fitted = CGRect(x: 10, y: 20, width: 100, height: 200)
        let projected = tutorProject(
            point: CGPoint(x: -50, y: 500),
            coordinateSpace: .init(width: 100, height: 200),
            fittedRect: fitted
        )
        XCTAssertEqual(projected, CGPoint(x: 10, y: 220))
    }

    func testRevealOrdersGroupsStaggersChildrenAndDropsUnknownShapes() throws {
        let json = #"{"type":"group","shapes":[{"type":"rect","reveal_order":2},{"type":"label","reveal_order":1},{"type":"future_shape","reveal_order":3}]}"#
        let group = try JSONDecoder().decode(TutorShape.self, from: Data(json.utf8))
        var state = TutorRevealState()
        let items = state.ingest(group)

        XCTAssertEqual(items.map { $0.shape.type }, ["label", "rect"])
        XCTAssertEqual(items.map(\.delayMs), [0, 700])
        XCTAssertEqual(state.items.count, 2)
    }

    func testClearRemovesPreviouslyRevealedShapes() throws {
        var state = TutorRevealState()
        _ = state.ingest(try decode(#"{"type":"rect"}"#))
        _ = state.ingest(try decode(#"{"type":"label"}"#))
        XCTAssertEqual(state.items.count, 2)
        _ = state.ingest(try decode(#"{"type":"clear"}"#))
        XCTAssertTrue(state.items.isEmpty)
    }

    func testIsSupportedIncludesEducationalPrimitivesAndCursive() {
        for type in ["angle_marker", "right_angle_marker", "square_on_segment", "arc",
                     "force_arrow", "free_body_body", "stack_frame", "flow_edge",
                     "field_line", "unit_label", "cursive_text", "curve", "freehand"] {
            XCTAssertTrue(TutorRevealState.isSupported(type), "\(type) should render")
        }
        XCTAssertFalse(TutorRevealState.isSupported("totally_unknown"))
    }

    func testEducationalPrimitiveShapesReachTheRevealPipeline() throws {
        var state = TutorRevealState()
        _ = state.ingest(try decode(#"{"type":"angle_marker","cx":1,"cy":1}"#))
        _ = state.ingest(try decode(#"{"type":"free_body_body","x":0,"y":0,"w":5,"h":5}"#))
        _ = state.ingest(try decode(#"{"type":"future_unknown"}"#))   // still dropped
        XCTAssertEqual(state.items.map { $0.shape.type }, ["angle_marker", "free_body_body"])
    }

    func testLabelLayoutDeCollidesOverlappingLabels() throws {
        let fitted = CGRect(x: 0, y: 0, width: 100, height: 100)
        let space = CGSize(width: 100, height: 100)   // 1:1 projection
        // Three labels stacked at nearly the same point → each pushed down a row.
        let shapes: [(id: UUID, shape: TutorShape)] = try [
            (UUID(), decode(#"{"type":"label","x":10,"y":10,"text":"A"}"#)),
            (UUID(), decode(#"{"type":"label","x":12,"y":10,"text":"B"}"#)),
            (UUID(), decode(#"{"type":"label","x":11,"y":11,"text":"C"}"#))
        ]
        let offsets = TutorLabelLayout.offsets(for: shapes, fittedRect: fitted, fallbackSpace: space)
        XCTAssertEqual(offsets[shapes[0].id], 0)                                  // first stays
        XCTAssertEqual(offsets[shapes[1].id], TutorLabelLayout.rowHeight)         // second pushed down
        XCTAssertEqual(offsets[shapes[2].id], TutorLabelLayout.rowHeight * 2)     // third pushed down again
    }

    func testLabelLayoutIgnoresNonTextShapes() throws {
        let shapes: [(id: UUID, shape: TutorShape)] = try [
            (UUID(), decode(#"{"type":"rect","x":10,"y":10,"w":5,"h":5}"#))
        ]
        XCTAssertTrue(TutorLabelLayout.offsets(
            for: shapes, fittedRect: CGRect(x: 0, y: 0, width: 100, height: 100),
            fallbackSpace: CGSize(width: 100, height: 100)).isEmpty)
    }

    private func decode(_ json: String) throws -> TutorShape {
        try JSONDecoder().decode(TutorShape.self, from: Data(json.utf8))
    }
}
