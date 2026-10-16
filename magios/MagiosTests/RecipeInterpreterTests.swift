import XCTest
@testable import Magician

/// Asserts the data-driven interpreter's computed (space-coordinate) geometry
/// matches the hand-coded native renderer for the educational primitives. The
/// interpreter exposes a `geometry(for:)` seam because a `GraphicsContext` can't
/// be inspected; the projection step is shared (`tutorProject`) and covered by
/// `TutorRevealTests`, so matching the space geometry proves render parity.
final class RecipeInterpreterTests: XCTestCase {
    private func recipe(_ json: String) throws -> TutorRecipe {
        try JSONDecoder().decode(TutorRecipe.self, from: Data(json.utf8))
    }

    private func shape(_ json: String) throws -> TutorShape {
        try JSONDecoder().decode(TutorShape.self, from: Data(json.utf8))
    }

    private func assertPoints(_ actual: [CGPoint], _ expected: [CGPoint],
                              accuracy: CGFloat = 1e-6, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(actual.count, expected.count, "point count", file: file, line: line)
        for (a, e) in zip(actual, expected) {
            XCTAssertEqual(a.x, e.x, accuracy: accuracy, file: file, line: line)
            XCTAssertEqual(a.y, e.y, accuracy: accuracy, file: file, line: line)
        }
    }

    // MARK: right_angle_marker — L polyline at the vertex

    func testRightAngleMarkerMatchesNativeLGeometry() throws {
        let r = try recipe(#"""
        {"type":"right_angle_marker","defaults":{"size":28},
         "draw":[{"op":"polyline","points":[["(x|cx)+size","y|cy"],["(x|cx)+size","(y|cy)+size"],["x|cx","(y|cy)+size"]]}]}
        """#)
        let s = try shape(#"{"type":"right_angle_marker","x":100,"y":200}"#)
        let geo = RecipeInterpreter.geometry(for: r, shape: s)

        XCTAssertEqual(geo.count, 1)
        XCTAssertEqual(geo[0].op, "polyline")
        // Native: (x+size,y) → (x+size,y+size) → (x,y+size), size default 28.
        assertPoints(geo[0].points, [
            CGPoint(x: 128, y: 200),
            CGPoint(x: 128, y: 228),
            CGPoint(x: 100, y: 228)
        ])
    }

    // MARK: angle_marker — 24-segment arc

    func testAngleMarkerMatchesNativeArcSampling() throws {
        let r = try recipe(#"""
        {"type":"angle_marker","aliases":["arc"],"defaults":{"size":36,"start_angle":0,"end_angle":90},
         "draw":[{"op":"arc","cx":"cx","cy":"cy","r":"r|size","from":"start_angle","to":"end_angle"}]}
        """#)
        let s = try shape(#"{"type":"angle_marker","cx":50,"cy":60}"#)
        let geo = RecipeInterpreter.geometry(for: r, shape: s)

        XCTAssertEqual(geo.count, 1)
        XCTAssertEqual(geo[0].op, "arc")

        // Native sampling: cx=50, cy=60, r=36 (size default), 0°→90°, 24 steps.
        let cx = 50.0, cy = 60.0, rr = 36.0
        let a1 = 0.0, a2 = Double.pi / 2
        var expected: [CGPoint] = []
        for i in 0...24 {
            let t = a1 + (a2 - a1) * Double(i) / 24.0
            expected.append(CGPoint(x: cx + cos(t) * rr, y: cy + sin(t) * rr))
        }
        assertPoints(geo[0].points, expected)
    }

    func testAngleMarkerUsesRadiusOverSize() throws {
        let r = try recipe(#"""
        {"type":"angle_marker","defaults":{"size":36},
         "draw":[{"op":"arc","cx":"cx","cy":"cy","r":"r|size","from":"start_angle","to":"end_angle"}]}
        """#)
        // r present → wins over size; missing angles default 0→90 in arcPoints.
        let s = try shape(#"{"type":"angle_marker","cx":0,"cy":0,"r":10}"#)
        let geo = RecipeInterpreter.geometry(for: r, shape: s)
        XCTAssertEqual(geo[0].points.first!, CGPoint(x: 10, y: 0))     // angle 0 → (r,0)
        XCTAssertEqual(geo[0].points.last!.x, 0, accuracy: 1e-6)       // angle 90 → (0,r)
        XCTAssertEqual(geo[0].points.last!.y, 10, accuracy: 1e-6)
    }

    // MARK: square_on_segment — polygon on the chosen side

    func testSquareOnSegmentMatchesNativeCornersLeftSide() throws {
        let r = try recipe(#"""
        {"type":"square_on_segment",
         "draw":[{"op":"polygon","points":[["sx","sy"],["ex","ey"],
           ["ex-(ey-sy)*side_sign","ey+(ex-sx)*side_sign"],
           ["sx-(ey-sy)*side_sign","sy+(ex-sx)*side_sign"]]}]}
        """#)
        // Segment (0,0)→(10,0), default side (left → sign +1).
        let s = try shape(#"{"type":"square_on_segment","x1":0,"y1":0,"x2":10,"y2":0}"#)
        let geo = RecipeInterpreter.geometry(for: r, shape: s)

        XCTAssertEqual(geo.count, 1)
        XCTAssertEqual(geo[0].op, "polygon")
        XCTAssertTrue(geo[0].closed)

        // Native left side: dx=10,dy=0,len=10,sign=1 → nx=0,ny=1.
        // corners: (0,0),(10,0),(10,10),(0,10)
        assertPoints(geo[0].points, [
            CGPoint(x: 0, y: 0),
            CGPoint(x: 10, y: 0),
            CGPoint(x: 10, y: 10),
            CGPoint(x: 0, y: 10)
        ])
    }

    func testSquareOnSegmentRightSideFlipsSign() throws {
        let r = try recipe(#"""
        {"type":"square_on_segment",
         "draw":[{"op":"polygon","points":[["sx","sy"],["ex","ey"],
           ["ex-(ey-sy)*side_sign","ey+(ex-sx)*side_sign"],
           ["sx-(ey-sy)*side_sign","sy+(ex-sx)*side_sign"]]}]}
        """#)
        // side "right" → sign -1 → square on the opposite side.
        let s = try shape(#"{"type":"square_on_segment","x1":0,"y1":0,"x2":10,"y2":0,"side":"right"}"#)
        let geo = RecipeInterpreter.geometry(for: r, shape: s)
        assertPoints(geo[0].points, [
            CGPoint(x: 0, y: 0),
            CGPoint(x: 10, y: 0),
            CGPoint(x: 10, y: -10),
            CGPoint(x: 0, y: -10)
        ])
    }

    // MARK: op skipped when a required coord is missing

    func testOpSkippedOnMissingCoordinate() throws {
        let r = try recipe(#"""
        {"type":"line","draw":[{"op":"line","from":["sx","sy"],"to":["ex","ey"]}]}
        """#)
        // No endpoint fields at all → sx/sy/ex/ey undefined → op produces nothing.
        let s = try shape(#"{"type":"line"}"#)
        XCTAssertTrue(RecipeInterpreter.geometry(for: r, shape: s).isEmpty)
    }

    // MARK: derived text_len for the callout background width

    func testCalloutBackgroundWidthUsesTextLen() throws {
        let r = try recipe(#"""
        {"type":"callout","draw":[
          {"op":"rect","x":"x-8","y":"y-18","w":"min(260,text_len*9+16)","h":36,"radius":9,"fill":true},
          {"op":"label","at":["x","y"],"text":"text","anchor":"leading"}]}
        """#)
        // 4-char text → width min(260, 4*9+16) = 52; rect corners (x-8,y-18)→(+52,+36).
        let s = try shape(#"{"type":"callout","x":100,"y":50,"text":"Next"}"#)
        let geo = RecipeInterpreter.geometry(for: r, shape: s)
        XCTAssertEqual(geo.count, 1)      // only the rect carries geometry; label omitted
        XCTAssertEqual(geo[0].op, "rect")
        assertPoints(geo[0].points, [
            CGPoint(x: 92, y: 32),                  // (100-8, 50-18)
            CGPoint(x: 92 + 52, y: 32 + 36)         // + (width, height)
        ])
    }

    // MARK: Shared golden fixtures — cross-platform parity with the TS interpreter

    private struct Fixture: Decodable {
        let name: String?
        let recipe: TutorRecipe
        let shape: TutorShape
        struct ExpectedOp: Decodable {
            let op: String
            let points: [[Double]]
            let closed: Bool?
        }
        let expected: [ExpectedOp]
    }

    /// The Swift interpreter must produce the SAME space-coordinate geometry the web
    /// interpreter asserts — `ui/.../recipeInterpreter.test.ts` loads these exact
    /// files. If the two interpreters drift, one of the two suites goes red.
    func testSharedGoldenFixturesMatchGeometry() throws {
        let dir = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()   // MagiosTests
            .deletingLastPathComponent()   // magios
            .deletingLastPathComponent()   // repo root
            .appendingPathComponent("docs/components/magician/tutor-primitive-fixtures")
        let files = try FileManager.default
            .contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
        XCTAssertFalse(files.isEmpty, "no golden fixtures at \(dir.path)")
        for file in files {
            let fx = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: file))
            let geo = RecipeInterpreter.geometry(for: fx.recipe, shape: fx.shape)
            let label = fx.name ?? file.lastPathComponent
            XCTAssertEqual(geo.count, fx.expected.count, "\(label): op count")
            for (g, e) in zip(geo, fx.expected) {
                XCTAssertEqual(g.op, e.op, "\(label): op")
                XCTAssertEqual(g.closed, e.closed ?? false, "\(label): closed (\(e.op))")
                XCTAssertEqual(g.points.count, e.points.count, "\(label): point count (\(e.op))")
                for (gp, ep) in zip(g.points, e.points) where ep.count == 2 {
                    XCTAssertEqual(Double(gp.x), ep[0], accuracy: 1e-4, "\(label): x (\(e.op))")
                    XCTAssertEqual(Double(gp.y), ep[1], accuracy: 1e-4, "\(label): y (\(e.op))")
                }
            }
        }
    }
}
