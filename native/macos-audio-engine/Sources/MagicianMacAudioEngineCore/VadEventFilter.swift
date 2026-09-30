import Foundation

public struct FilteredVadResult: Sendable {
    public let events: [AudioEngineStreamEvent]
    public let forceReset: Bool
}

/// Adds minimum-speech and maximum-utterance guarantees around FluidAudio's
/// probability hysteresis, whose streaming state emits boundaries immediately.
public struct VadEventFilter: Sendable {
    private let minSpeechSamples: Int
    private let maxSpeechSamples: Int
    private let speechThreshold: Float
    private var pendingStartSample: Int?
    private var pendingStartObservedAtSample: Int?
    private var emittedStartSample: Int?

    public init(minSpeechMs: UInt64, maxUtteranceMs: UInt64, speechThreshold: Float = 0.65) {
        minSpeechSamples = Int(minSpeechMs) * PCMNormalizer.outputSampleRate / 1_000
        maxSpeechSamples = max(1, Int(maxUtteranceMs) * PCMNormalizer.outputSampleRate / 1_000)
        self.speechThreshold = speechThreshold
    }

    public mutating func consume(
        probability: Float,
        processedSamples: Int,
        rawStartSample: Int? = nil,
        rawEndSample: Int? = nil
    ) -> FilteredVadResult {
        var events: [AudioEngineStreamEvent] = [
            .probability(value: probability, atMs: milliseconds(processedSamples))
        ]
        if let rawStartSample, pendingStartSample == nil, emittedStartSample == nil {
            pendingStartSample = max(0, rawStartSample)
            pendingStartObservedAtSample = processedSamples
        }
        if let pending = pendingStartSample,
            processedSamples - pending >= minSpeechSamples,
            minSpeechSamples == 0 || processedSamples > (pendingStartObservedAtSample ?? processedSamples),
            probability >= speechThreshold,
            rawEndSample == nil
        {
            emittedStartSample = pending
            pendingStartSample = nil
            pendingStartObservedAtSample = nil
            events.append(.speechStarted(atMs: milliseconds(pending)))
        }
        if let rawEndSample {
            pendingStartSample = nil
            pendingStartObservedAtSample = nil
            if emittedStartSample != nil {
                events.append(.speechEnded(atMs: milliseconds(max(0, rawEndSample))))
            }
            emittedStartSample = nil
        }
        if let start = emittedStartSample, processedSamples - start >= maxSpeechSamples {
            events.append(.speechEnded(atMs: milliseconds(processedSamples)))
            emittedStartSample = nil
            pendingStartSample = nil
            pendingStartObservedAtSample = nil
            return FilteredVadResult(events: events, forceReset: true)
        }
        return FilteredVadResult(events: events, forceReset: false)
    }

    public mutating func finish(processedSamples: Int) -> [AudioEngineStreamEvent] {
        pendingStartSample = nil
        pendingStartObservedAtSample = nil
        defer { emittedStartSample = nil }
        guard emittedStartSample != nil else { return [] }
        return [.speechEnded(atMs: milliseconds(processedSamples))]
    }

    private func milliseconds(_ sample: Int) -> UInt64 {
        UInt64(max(0, sample)) * 1_000 / UInt64(PCMNormalizer.outputSampleRate)
    }
}
