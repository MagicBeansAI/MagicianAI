import XCTest
@testable import Magician

/// The pre-roll buffer is the only thing standing between the user and losing
/// the first second of every command, so its ordering and overwrite semantics
/// are pinned here rather than discovered on device.
final class WakePreRollTests: XCTestCase {

    private func bytes(_ values: [UInt8]) -> Data { Data(values) }

    /// Mirrors the `VoicePCMTests` helper: reinterpret Data (int16-LE) as [Int16].
    private func int16s(_ data: Data) -> [Int16] {
        stride(from: 0, to: data.count - 1, by: 2).map { i in
            Int16(bitPattern: UInt16(data[data.startIndex + i]) | (UInt16(data[data.startIndex + i + 1]) << 8))
        }
    }

    /// The inverse: PCM16 samples as the int16-LE bytes a capture buffer hands over.
    private func pcm16(_ samples: [Int16]) -> Data {
        var data = Data(capacity: samples.count * 2)
        for sample in samples {
            let bits = UInt16(bitPattern: sample)
            data.append(UInt8(bits & 0x00FF))
            data.append(UInt8(bits >> 8))
        }
        return data
    }

    func testDrainOnAnEmptyBufferReturnsNothing() {
        var buffer = WakePreRoll(capacityBytes: 8)
        XCTAssertTrue(buffer.isEmpty)
        XCTAssertEqual(buffer.drain(), Data())
    }

    func testUnderfilledBufferDrainsExactlyWhatWentIn() {
        var buffer = WakePreRoll(capacityBytes: 8)
        buffer.append(bytes([1, 2, 3, 4]))
        XCTAssertEqual(buffer.count, 4)
        XCTAssertEqual(buffer.drain(), bytes([1, 2, 3, 4]))
    }

    func testAppendsAccumulateInOrderAcrossCalls() {
        var buffer = WakePreRoll(capacityBytes: 8)
        buffer.append(bytes([1, 2]))
        buffer.append(bytes([3, 4]))
        XCTAssertEqual(buffer.drain(), bytes([1, 2, 3, 4]))
    }

    /// Wraparound is the case that silently corrupts audio if the read offset is
    /// wrong: the drain must still be oldest-to-newest, not storage order.
    func testOverfillKeepsOnlyTheMostRecentBytesOldestFirst() {
        var buffer = WakePreRoll(capacityBytes: 4)
        buffer.append(bytes([1, 2, 3, 4]))
        buffer.append(bytes([5, 6]))
        XCTAssertEqual(buffer.count, 4)
        XCTAssertEqual(buffer.drain(), bytes([3, 4, 5, 6]))
    }

    /// A single chunk larger than the whole buffer must keep its TAIL — the most
    /// recent audio — not its head.
    func testSingleOversizedAppendKeepsItsTail() {
        var buffer = WakePreRoll(capacityBytes: 4)
        buffer.append(bytes([1, 2, 3, 4, 5, 6]))
        XCTAssertEqual(buffer.drain(), bytes([3, 4, 5, 6]))
    }

    /// `data.count > capacityBytes` is the classic off-by-one: an append of EXACTLY
    /// capacity is not oversized and must survive whole — into a fresh buffer, and
    /// across a wrap where it evicts everything already held.
    func testAppendOfExactlyCapacityIsNotTreatedAsOversized() {
        var fresh = WakePreRoll(capacityBytes: 4)
        fresh.append(bytes([1, 2, 3, 4]))
        XCTAssertEqual(fresh.count, 4)
        XCTAssertEqual(fresh.drain(), bytes([1, 2, 3, 4]))

        var wrapped = WakePreRoll(capacityBytes: 4)
        wrapped.append(bytes([9, 9]))
        wrapped.append(bytes([1, 2, 3, 4]))
        XCTAssertEqual(wrapped.drain(), bytes([1, 2, 3, 4]))
    }

    func testDrainResetsSoTheNextTurnStartsClean() {
        var buffer = WakePreRoll(capacityBytes: 4)
        buffer.append(bytes([1, 2]))
        _ = buffer.drain()
        XCTAssertTrue(buffer.isEmpty)
        XCTAssertEqual(buffer.drain(), Data())
    }

    func testResetDiscardsWithoutDraining() {
        var buffer = WakePreRoll(capacityBytes: 4)
        buffer.append(bytes([1, 2]))
        buffer.reset()
        XCTAssertEqual(buffer.drain(), Data())
    }

    /// The shipping configuration: 2 s of 16 kHz mono PCM16 = 64000 bytes.
    func testSecondsInitialiserComputesPcm16Capacity() {
        let buffer = WakePreRoll(seconds: 2, sampleRate: 16_000)
        XCTAssertEqual(buffer.capacityBytes, 64_000)
    }

    /// Every other case asserts raw bytes, which cannot see sample alignment. Because
    /// the type enforces an even capacity AND even appends, the wrap always lands on a
    /// sample boundary and real PCM16 survives an overfilling drain intact. An odd
    /// count through either door would start the drain mid-sample and byte-swap every
    /// value into plausible-sounding noise.
    func testOverfilledPcm16SamplesRoundTripOldestFirst() {
        var buffer = WakePreRoll(capacityBytes: 6)   // 3 samples
        buffer.append(pcm16([1000, 2000]))
        buffer.append(pcm16([3000, 4000]))
        XCTAssertEqual(int16s(buffer.drain()), [2000, 3000, 4000])
    }
}
