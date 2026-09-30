import SwiftUI
import XCTest
@testable import Magician

/// The blob's geometry contract. The shape renders on surfaces that cannot be
/// tested (archived WidgetKit SwiftUI), so everything that matters about it —
/// closedness, staying inside its rect, determinism, and per-phase
/// distinctness — is proven here as pure geometry, off any surface.
final class AuroraBlobShapeTests: XCTestCase {

    private let rect = CGRect(x: 0, y: 0, width: 120, height: 120)

    /// The seeds the orb actually ships: the five live presets plus graphite's 0.
    private var presetSeeds: [Double] {
        [AuroraPalette.armedEmber, .violetSurge, .calmAurora,
         .amberThinking, .tealSpeaking, .graphite].map(\.blobSeed)
    }

    /// A blob that fails to close would leak its fill on some renderers and
    /// stroke an open arc on others; every seed must produce a closed loop.
    func testPathIsNonEmptyAndClosedForEveryPresetSeed() {
        for seed in presetSeeds {
            let path = AuroraBlobShape(seed: seed, morph: 0, amplitude: 0.10).path(in: rect)
            XCTAssertFalse(path.isEmpty, "seed \(seed) drew nothing")
            var elements: [Path.Element] = []
            path.forEach { elements.append($0) }
            guard case .closeSubpath = elements.last else {
                return XCTFail("seed \(seed) does not close its subpath")
            }
        }
    }

    /// The fixed-footprint contract: callers size the blob with a frame and
    /// trust nothing to overflow it, at the resting amplitude, at the clamp
    /// ceiling, and for an out-of-range amplitude the clamp must absorb.
    func testTheBlobStaysInsideItsRectAtAmplitudeExtremes() {
        for amplitude in [0.0, 0.12, 1.0, 1.4] {
            for seed in presetSeeds {
                let bounds = AuroraBlobShape(seed: seed, morph: 0, amplitude: amplitude)
                    .path(in: rect).boundingRect
                XCTAssertTrue(rect.insetBy(dx: -0.001, dy: -0.001).contains(bounds),
                              "seed \(seed) amplitude \(amplitude) escapes: \(bounds)")
            }
        }
    }

    /// Same seed, same blob, forever — the property the whole out-of-process
    /// treatment leans on: a phase must re-render its silhouette identically
    /// across publishes, or the orb would wobble on every content update.
    func testSameSeedAndMorphProduceTheIdenticalPath() {
        let first = points(seed: 2.1, morph: 0.7, amplitude: 0.10)
        let second = points(seed: 2.1, morph: 0.7, amplitude: 0.10)
        XCTAssertEqual(first, second)
        XCTAssertEqual(
            AuroraBlobShape(seed: 2.1, morph: 0.7, amplitude: 0.10).path(in: rect).boundingRect,
            AuroraBlobShape(seed: 2.1, morph: 0.7, amplitude: 0.10).path(in: rect).boundingRect
        )
    }

    /// Each phase owns a distinct still silhouette — that distinctness IS the
    /// phase morph out-of-process, so two seeds collapsing to near-identical
    /// geometry would erase a phase change the identity swap promises to show.
    func testDistinctPhaseSeedsProduceMeasurablyDifferentSilhouettes() {
        let liveSeeds = [AuroraPalette.armedEmber, .violetSurge, .calmAurora,
                         .amberThinking, .tealSpeaking].map(\.blobSeed)
        for (index, seed) in liveSeeds.enumerated() {
            for other in liveSeeds[(index + 1)...] {
                let a = points(seed: seed, morph: 0, amplitude: 0.10)
                let b = points(seed: other, morph: 0, amplitude: 0.10)
                XCTAssertEqual(a.count, b.count)
                let separation = zip(a, b).map { hypot($0.x - $1.x, $0.y - $1.y) }.max() ?? 0
                XCTAssertGreaterThan(separation, 2,
                                     "seeds \(seed) and \(other) draw nearly the same blob")
            }
        }
    }

    /// Amplitude 0 is a perfect circle — the ended orb's rule: stillness and
    /// geometry both say "over", so graphite must not keep a lobe.
    func testZeroAmplitudeIsAPerfectCircle() {
        let center = CGPoint(x: rect.midX, y: rect.midY)
        let radii = points(seed: 0, morph: 0, amplitude: 0)
            .map { hypot($0.x - center.x, $0.y - center.y) }
        let expected = rect.width / 2
        for radius in radii {
            XCTAssertEqual(Double(radius), Double(expected), accuracy: 0.001)
        }
    }

    /// The in-process channel: advancing `morph` must actually move the lobes,
    /// or the beacon's render loop would be spending frames on a still image.
    func testAdvancingMorphMovesTheLobes() {
        let still = points(seed: 3.3, morph: 0, amplitude: 0.10)
        let advanced = points(seed: 3.3, morph: 1.3, amplitude: 0.10)
        let separation = zip(still, advanced).map { hypot($0.x - $1.x, $0.y - $1.y) }.max() ?? 0
        XCTAssertGreaterThan(separation, 1, "morph advanced but the silhouette did not")
    }

    private func points(seed: Double, morph: Double, amplitude: Double) -> [CGPoint] {
        var collected: [CGPoint] = []
        AuroraBlobShape(seed: seed, morph: morph, amplitude: amplitude)
            .path(in: rect)
            .forEach { element in
                switch element {
                case .move(let point), .line(let point):
                    collected.append(point)
                default:
                    break
                }
            }
        return collected
    }
}
