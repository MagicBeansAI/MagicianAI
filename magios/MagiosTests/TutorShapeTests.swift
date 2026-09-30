import XCTest
@testable import Magician

final class TutorShapeTests: XCTestCase {
    func testDecodesShapeFamiliesAndSnakeCaseMetadata() throws {
        let fixtures = [
            #"{"type":"rect","x":10,"y":20,"w":30,"h":40,"coordinate_space":{"width":100,"height":200},"storyboard_step_id":"s1","duration_ms":600}"#,
            #"{"type":"arrow","from_x":10,"from_y":20,"to_x":80,"to_y":90,"stroke_width":4}"#,
            #"{"type":"label","x":20,"y":30,"text":"Tap here","tutor_step_label":"Compose"}"#,
            #"{"type":"circle","cx":50,"cy":60,"r":12}"#,
            #"{"type":"path","points":[[1,2],[3,4]],"delay_ms":100}"#
        ]
        let decoded = try fixtures.map { try JSONDecoder().decode(TutorShape.self, from: Data($0.utf8)) }

        XCTAssertEqual(decoded.map(\.type), ["rect", "arrow", "label", "circle", "path"])
        XCTAssertEqual(decoded[0].coordinateSpace, .init(width: 100, height: 200))
        XCTAssertEqual(decoded[0].storyboardStepId, "s1")
        XCTAssertEqual(decoded[1].startX, 10)
        XCTAssertEqual(decoded[2].caption, "Compose")
        XCTAssertEqual(decoded[4].points, [.init(x: 1, y: 2), .init(x: 3, y: 4)])
    }

    func testDecodesBackendCaptureSpaceAndGroupChildren() throws {
        let json = #"{"type":"group","coordinate_space":"capture","capture_image_size":{"width":1179,"height":2556},"shapes":[{"type":"highlight","x":1,"y":2,"width":3,"height":4,"reveal_order":2},{"type":"label","x":4,"y":5,"text":"Next","reveal_order":1}]}"#
        let shape = try JSONDecoder().decode(TutorShape.self, from: Data(json.utf8))

        XCTAssertEqual(shape.coordinateSpaceName, "capture")
        XCTAssertEqual(shape.sourceSpace, .init(width: 1179, height: 2556))
        XCTAssertEqual(shape.children?.count, 2)
        XCTAssertEqual(shape.children?.first?.rectWidth, 3)
    }

    func testUnknownTypeStillDecodesForForwardCompatibility() throws {
        let shape = try JSONDecoder().decode(TutorShape.self, from: Data(#"{"type":"future_shape","x":1}"#.utf8))
        XCTAssertEqual(shape.type, "future_shape")
    }

    func testDecodesSemanticBoxLabelsAndFormulaFallbacks() throws {
        let fixtures = [
            #"{"type":"flow_node","x":150,"y":260,"w":250,"h":120,"label":"motion"}"#,
            #"{"type":"state_box","x":125,"y":570,"w":300,"h":100,"label":"Physics: velocity, acceleration"}"#,
            #"{"type":"formula","x":350,"y":760,"formula":"f′(x) = 0"}"#
        ]
        let decoded = try fixtures.map { try JSONDecoder().decode(TutorShape.self, from: Data($0.utf8)) }

        XCTAssertEqual(decoded.map(\.displayText), ["motion", "Physics: velocity, acceleration", "f′(x) = 0"])
        XCTAssertEqual(decoded[0].label, "motion")
        XCTAssertEqual(decoded[2].formula, "f′(x) = 0")
    }

    func testWaitForVoiceDecodesForNarratedSteps() throws {
        let shape = try JSONDecoder().decode(
            TutorShape.self,
            from: Data(#"{"type":"label","wait_for_voice":true,"narration":"Listen first"}"#.utf8)
        )
        XCTAssertEqual(shape.waitForVoice, true)
        XCTAssertEqual(shape.narration, "Listen first")
    }

    func testDecodesEducationalPrimitiveGeometry() throws {
        let marker = try JSONDecoder().decode(TutorShape.self, from: Data(
            #"{"type":"angle_marker","cx":10,"cy":20,"size":36,"start_angle":0,"end_angle":90}"#.utf8))
        XCTAssertEqual(marker.size, 36); XCTAssertEqual(marker.startAngle, 0); XCTAssertEqual(marker.endAngle, 90)

        let square = try JSONDecoder().decode(TutorShape.self, from: Data(
            #"{"type":"square_on_segment","x1":0,"y1":0,"x2":10,"y2":0,"side":"right"}"#.utf8))
        XCTAssertEqual(square.side, "right"); XCTAssertEqual(square.startX, 0); XCTAssertEqual(square.endX, 10)

        let cursive = try JSONDecoder().decode(TutorShape.self, from: Data(
            #"{"type":"cursive_text","x":5,"y":5,"text":"hi","font_size":80}"#.utf8))
        XCTAssertEqual(cursive.fontSize, 80)

        // curve control-point aliases (control1_x maps into c1x)
        let curve = try JSONDecoder().decode(TutorShape.self, from: Data(
            #"{"type":"curve","x1":0,"y1":0,"x2":10,"y2":10,"control1_x":3,"control1_y":7}"#.utf8))
        XCTAssertEqual(curve.c1x, 3); XCTAssertEqual(curve.c1y, 7)
    }

    func testDecodesLifecycleFields() throws {
        let shape = try JSONDecoder().decode(TutorShape.self, from: Data(
            #"{"type":"rect","ttl_ms":25000,"persist":true,"persist_until_step":"s3","clear_previous":true}"#.utf8))
        XCTAssertEqual(shape.ttlMs, 25000)
        XCTAssertEqual(shape.persist, true)
        XCTAssertEqual(shape.persistUntilStep, "s3")
        XCTAssertEqual(shape.clearPrevious, true)
    }
}
