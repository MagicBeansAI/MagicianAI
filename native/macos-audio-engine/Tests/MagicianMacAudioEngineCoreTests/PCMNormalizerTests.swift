import Foundation
@testable import MagicianMacAudioEngineCore
import XCTest

final class PCMNormalizerTests: XCTestCase {
    func testSplitStereoPCM16FramesArePreservedAndMixed() throws {
        var normalizer = try PCMNormalizer(
            format: .init(sampleRateHz: 16_000, channels: 2, sampleFormat: .pcmS16Le)
        )
        let samples: [Int16] = [16_384, -16_384, 8_192, 8_192, -8_192, -8_192]
        let data = Data(samples.flatMap { value in
            let raw = UInt16(bitPattern: value)
            return [UInt8(raw & 0xff), UInt8(raw >> 8)]
        })
        XCTAssertTrue(try normalizer.append(data.prefix(3)).isEmpty)
        let output = try normalizer.append(data.dropFirst(3)) + normalizer.finish()
        XCTAssertEqual(output.count, 3)
        XCTAssertEqual(output[0], 0, accuracy: 0.0001)
        XCTAssertEqual(output[1], 0.25, accuracy: 0.0001)
        XCTAssertEqual(output[2], -0.25, accuracy: 0.0001)
    }

    func testEightKilohertzInputUpsamplesWithoutDroppingDuration() throws {
        var normalizer = try PCMNormalizer(
            format: .init(sampleRateHz: 8_000, channels: 1, sampleFormat: .pcmF32Le)
        )
        let input = [Float](repeating: 0.5, count: 800)
        var data = Data()
        for sample in input {
            var bits = sample.bitPattern.littleEndian
            withUnsafeBytes(of: &bits) { data.append(contentsOf: $0) }
        }
        let output = try normalizer.append(data) + normalizer.finish()
        XCTAssertEqual(output.count, 1_600, accuracy: 2)
        XCTAssertTrue(output.allSatisfy { abs($0 - 0.5) < 0.0001 })
    }

    func testNonFiniteFloatInputIsSanitized() throws {
        var normalizer = try PCMNormalizer(
            format: .init(sampleRateHz: 16_000, channels: 1, sampleFormat: .pcmF32Le)
        )
        let values: [Float] = [.nan, .infinity, -.infinity, 2, -2]
        var data = Data()
        for value in values {
            var bits = value.bitPattern.littleEndian
            withUnsafeBytes(of: &bits) { data.append(contentsOf: $0) }
        }
        let output = try normalizer.append(data) + normalizer.finish()
        XCTAssertTrue(output.allSatisfy(\.isFinite))
        XCTAssertTrue(output.allSatisfy { (-1...1).contains($0) })
    }
}

