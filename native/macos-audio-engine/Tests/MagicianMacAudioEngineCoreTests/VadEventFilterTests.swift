@testable import MagicianMacAudioEngineCore
import XCTest

final class VadEventFilterTests: XCTestCase {
    func testShortBurstProducesNoBoundaryEvents() {
        var filter = VadEventFilter(minSpeechMs: 250, maxUtteranceMs: 10_000)
        _ = filter.consume(probability: 0.9, processedSamples: 4_096, rawStartSample: 0)
        let result = filter.consume(
            probability: 0.1,
            processedSamples: 4_096,
            rawEndSample: 3_000
        )
        XCTAssertFalse(result.events.contains { event in
            if case .speechStarted = event { return true }
            if case .speechEnded = event { return true }
            return false
        })
    }

    func testMinimumSpeechDefersStartAndPreservesOriginalTimestamp() {
        var filter = VadEventFilter(minSpeechMs: 250, maxUtteranceMs: 10_000)
        let first = filter.consume(probability: 0.9, processedSamples: 2_000, rawStartSample: 1_000)
        XCTAssertFalse(first.events.contains { if case .speechStarted = $0 { true } else { false } })
        let second = filter.consume(probability: 0.9, processedSamples: 5_000)
        XCTAssertTrue(second.events.contains(.speechStarted(atMs: 62)))
        let third = filter.consume(probability: 0.1, processedSamples: 8_000, rawEndSample: 7_000)
        XCTAssertTrue(third.events.contains(.speechEnded(atMs: 437)))
    }

    func testMaximumUtteranceForcesEndAndReset() {
        var filter = VadEventFilter(minSpeechMs: 0, maxUtteranceMs: 500)
        let first = filter.consume(probability: 0.9, processedSamples: 1, rawStartSample: 0)
        XCTAssertTrue(first.events.contains(.speechStarted(atMs: 0)))
        let forced = filter.consume(probability: 0.9, processedSamples: 8_001)
        XCTAssertTrue(forced.forceReset)
        XCTAssertTrue(forced.events.contains(.speechEnded(atMs: 500)))
    }
}

