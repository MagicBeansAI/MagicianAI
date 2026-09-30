import FluidAudio
import Foundation
import MagicianMacAudioEngineCore

private struct EmittedSpeakerSegment: Equatable {
    let startMs: UInt64
    let endMs: UInt64
    let finalized: Bool
}

struct DiarizationStreamProcessor {
    private let pipeline: SortformerDiarizer
    private var normalizer: PCMNormalizer
    private var emitted: [String: EmittedSpeakerSegment] = [:]

    init(
        models: SortformerModels,
        format: StreamAudioFormat,
        configuration: DiarizationSessionConfiguration
    ) throws {
        if let expected = configuration.expectedSpeakers, !(1...4).contains(expected) {
            throw StreamConfigurationError.invalid(
                "FluidAudio Sortformer expected_speakers must be between 1 and 4"
            )
        }
        self.normalizer = try PCMNormalizer(format: format)
        self.pipeline = SortformerDiarizer(config: .default)
        pipeline.initialize(models: models)
    }

    mutating func append(_ data: Data) throws -> [AudioEngineStreamEvent] {
        let samples = try normalizer.append(data)
        if !samples.isEmpty { _ = try pipeline.processSamples(samples) }
        return collectTimelineEvents()
    }

    mutating func finish() throws -> [AudioEngineStreamEvent] {
        let tail = normalizer.finish()
        if !tail.isEmpty { _ = try pipeline.processSamples(tail) }
        pipeline.timeline.finalize()
        return collectTimelineEvents()
    }

    private mutating func collectTimelineEvents() -> [AudioEngineStreamEvent] {
        var next: [String: EmittedSpeakerSegment] = [:]
        var events: [AudioEngineStreamEvent] = []
        for speaker in pipeline.timeline.segments.indices {
            let allSegments = pipeline.timeline.segments[speaker]
                + pipeline.timeline.tentativeSegments[speaker]
            for segment in allSegments {
                let speakerId = "speaker_\(speaker + 1)"
                let startMs = UInt64(max(0, segment.startTime) * 1_000)
                let endMs = UInt64(max(segment.startTime, segment.endTime) * 1_000)
                let key = "\(speakerId):\(segment.startFrame)"
                let current = EmittedSpeakerSegment(
                    startMs: startMs,
                    endMs: endMs,
                    finalized: segment.isFinalized
                )
                next[key] = current
                if let previous = emitted[key] {
                    if previous != current {
                        events.append(.segmentRevised(
                            speakerId: speakerId,
                            startMs: startMs,
                            endMs: endMs,
                            confidence: nil
                        ))
                    }
                    if !previous.finalized && current.finalized {
                        events.append(.speakerEnded(speakerId: speakerId, atMs: endMs))
                    }
                } else {
                    events.append(.speakerStarted(speakerId: speakerId, atMs: startMs))
                    events.append(.segmentRevised(
                        speakerId: speakerId,
                        startMs: startMs,
                        endMs: endMs,
                        confidence: nil
                    ))
                    if current.finalized {
                        events.append(.speakerEnded(speakerId: speakerId, atMs: endMs))
                    }
                }
            }
        }
        emitted = next
        return events
    }
}
