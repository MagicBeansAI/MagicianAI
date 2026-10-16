import XCTest
@testable import Magician

/// The blackboard model space is a port of the web `refreshCoordinateSpace`: the
/// largest side is clamped to 2048, aspect preserved, so source-free shapes project
/// 1:1 onto the overlay canvas.
final class TutorBlackboardCoordinateTests: XCTestCase {
    func testViewportUnderCapIsUnchanged() {
        let size = TutorBlackboardCanvas.modelSize(viewport: CGSize(width: 390, height: 844))
        XCTAssertEqual(size.width, 390, accuracy: 0.001)
        XCTAssertEqual(size.height, 844, accuracy: 0.001)
    }

    func testViewportOverCapScalesDownPreservingAspect() {
        let size = TutorBlackboardCanvas.modelSize(viewport: CGSize(width: 3000, height: 1500))
        XCTAssertEqual(size.width, 2048, accuracy: 0.001)   // largest side clamped
        XCTAssertEqual(size.height, 1024, accuracy: 0.001)  // 2:1 aspect preserved
    }

    func testZeroViewportGuardsToOne() {
        let size = TutorBlackboardCanvas.modelSize(viewport: .zero)
        XCTAssertEqual(size.width, 1, accuracy: 0.001)
        XCTAssertEqual(size.height, 1, accuracy: 0.001)
    }
}
