import XCTest
@testable import Magician

final class VoicePCMTests: XCTestCase {
    // reinterpret Data (int16-LE) as [Int16] for assertions
    private func int16s(_ data: Data) -> [Int16] {
        stride(from: 0, to: data.count - 1, by: 2).map { i in
            Int16(bitPattern: UInt16(data[data.startIndex + i]) | (UInt16(data[data.startIndex + i + 1]) << 8))
        }
    }

    func testFloatToInt16ClampsAndScales() {
        XCTAssertEqual(int16s(VoicePCM.floatToInt16LE([0, 1.0, -1.0, 2.0, -2.0])),
                       [0, 32767, -32768, 32767, -32768])   // clamped
    }
    func testRoundTripApprox() {
        let floats: [Float] = [0, 0.5, -0.5, 0.25]
        let back = VoicePCM.int16ToFloat(VoicePCM.floatToInt16LE(floats))
        for (a, b) in zip(floats, back) { XCTAssertEqual(a, b, accuracy: 0.001) }
    }
    func testLittleEndianByteOrder() {
        // 0.5 → 16383 (0x3FFF) → bytes [0xFF, 0x3F]
        XCTAssertEqual([UInt8](VoicePCM.floatToInt16LE([0.5])), [0xFF, 0x3F])
    }

    // MARK: - resamplePCM16LE
    //
    // The ambient wake pre-roll is 16 kHz because the wake model is; this transport
    // is 24 kHz and does not negotiate. Getting the conversion wrong is silent — the
    // bytes are still bytes — so the arithmetic is pinned here rather than heard on a
    // device.

    private func pcm16(_ samples: [Int16]) -> Data {
        var data = Data(capacity: samples.count * 2)
        for sample in samples {
            let value = UInt16(bitPattern: sample)
            data.append(UInt8(value & 0xFF))
            data.append(UInt8(value >> 8))
        }
        return data
    }

    /// 2 s of pre-roll must stay 2 s of audio: 16 kHz → 24 kHz is 1.5x the samples.
    func testUpsampleGrowsByTheRateRatio() {
        let input = pcm16([Int16](repeating: 1_000, count: 16_000 * 2))
        let output = VoicePCM.resamplePCM16LE(input, from: 16_000, to: 24_000)
        XCTAssertEqual(output.count, 24_000 * 2 * 2)
    }

    /// Whole PCM16 samples, end to end — the same guarantee `floatToInt16LE` gives.
    /// An odd byte anywhere byte-swaps every value after it into plausible noise.
    func testOutputByteCountIsAlwaysEven() {
        for sampleCount in 1...9 {
            let input = pcm16([Int16](repeating: 7, count: sampleCount))
            for (from, to) in [(16_000, 24_000), (24_000, 16_000), (16_000, 44_100)] {
                let output = VoicePCM.resamplePCM16LE(input, from: from, to: to)
                XCTAssertTrue(output.count.isMultiple(of: 2),
                              "\(sampleCount) samples \(from)→\(to) produced \(output.count) bytes")
            }
        }
    }

    /// An odd input length is a caller's mistake, not half a sample: it is floored
    /// rather than trusted, so it cannot launder a misalignment downstream.
    func testOddInputLengthIsFlooredToWholeSamples() {
        var input = pcm16([100, 200, 300])
        input.append(0x55)
        let output = VoicePCM.resamplePCM16LE(input, from: 16_000, to: 24_000)
        // 3 whole samples, not 3.5 → round(3 * 1.5) = 5 samples out.
        XCTAssertEqual(output.count, 10)
    }

    /// A constant signal must stay constant — interpolation between equal
    /// neighbours cannot invent a slope, and a sign or endianness slip here would
    /// show up as a value that is not the input.
    func testConstantSignalSurvivesUnchanged() {
        let input = pcm16([Int16](repeating: -12_345, count: 64))
        let output = VoicePCM.resamplePCM16LE(input, from: 16_000, to: 24_000)
        XCTAssertEqual(Set(int16s(output)), [-12_345])
    }

    /// A rising ramp must stay monotonically non-decreasing: an off-by-one in the
    /// index arithmetic reads as a sawtooth, which is audible but only on a device.
    func testRampStaysMonotonic() {
        let input = pcm16((0..<64).map { Int16($0 * 100) })
        let output = int16s(VoicePCM.resamplePCM16LE(input, from: 16_000, to: 24_000))
        XCTAssertEqual(output.count, 96)
        for (previous, next) in zip(output, output.dropFirst()) {
            XCTAssertLessThanOrEqual(previous, next)
        }
        XCTAssertEqual(output.first, 0)
    }

    /// Equal rates copy rather than interpolate, so the in-app path can share this
    /// function without paying for a conversion it does not need.
    func testEqualRatesPassThroughUnchanged() {
        let input = pcm16([1, -2, 3, -4])
        XCTAssertEqual(VoicePCM.resamplePCM16LE(input, from: 24_000, to: 24_000), input)
    }

    func testEmptyAndDegenerateInputsProduceNothing() {
        XCTAssertTrue(VoicePCM.resamplePCM16LE(Data(), from: 16_000, to: 24_000).isEmpty)
        XCTAssertTrue(VoicePCM.resamplePCM16LE(Data([0x01]), from: 16_000, to: 24_000).isEmpty)
        XCTAssertTrue(VoicePCM.resamplePCM16LE(pcm16([1, 2]), from: 0, to: 24_000).isEmpty)
        XCTAssertTrue(VoicePCM.resamplePCM16LE(pcm16([1, 2]), from: 16_000, to: 0).isEmpty)
    }

    /// The pre-roll ring hands over a `Data` produced by `drain()`; a sliced value
    /// keeps its parent's indices, and byte-wise reads that ignored that would
    /// resample the wrong region.
    func testSlicedInputIsResampledFromItsOwnStart() {
        let full = pcm16([9_999, 9_999, 500, 500, 500, 500])
        let slice = full[full.index(full.startIndex, offsetBy: 4)...]
        let output = int16s(VoicePCM.resamplePCM16LE(slice, from: 16_000, to: 24_000))
        XCTAssertEqual(Set(output), [500])
    }
}
