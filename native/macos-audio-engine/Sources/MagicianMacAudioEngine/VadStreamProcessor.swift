import FluidAudio
import Foundation
import MagicianMacAudioEngineCore

private enum VadStreamProcessorError: Error, LocalizedError {
    case invalidConfiguration(String)

    var errorDescription: String? {
        switch self { case .invalidConfiguration(let reason): reason }
    }
}

struct VadStreamProcessor {
    private let manager: VadManager
    private let segmentation: VadSegmentationConfig
    private var streamState: VadStreamState
    private var normalizer: PCMNormalizer
    private var pendingSamples: [Float] = []
    private var filter: VadEventFilter
    private var processedSamples = 0

    init(
        manager: VadManager,
        format: StreamAudioFormat,
        configuration: VadSessionConfiguration
    ) async throws {
        let maximumBoundaryMs: UInt64 = 24 * 60 * 60 * 1_000
        guard configuration.threshold.isFinite, (0...1).contains(configuration.threshold) else {
            throw VadStreamProcessorError.invalidConfiguration(
                "VAD threshold must be finite and between 0 and 1"
            )
        }
        guard configuration.maxUtteranceMs > 0,
            configuration.minSpeechMs <= configuration.maxUtteranceMs,
            configuration.maxUtteranceMs <= maximumBoundaryMs,
            configuration.minSilenceMs <= maximumBoundaryMs,
            configuration.preRollMs <= maximumBoundaryMs,
            configuration.hangoverMs <= maximumBoundaryMs
        else {
            throw VadStreamProcessorError.invalidConfiguration(
                "VAD timing values must be bounded to 24 hours"
            )
        }
        self.manager = manager
        self.normalizer = try PCMNormalizer(format: format)
        self.streamState = await manager.makeStreamState()
        self.filter = VadEventFilter(
            minSpeechMs: configuration.minSpeechMs,
            maxUtteranceMs: configuration.maxUtteranceMs,
            speechThreshold: configuration.threshold
        )
        let maxSeconds = max(0.001, Double(configuration.maxUtteranceMs) / 1_000)
        let (combinedSilenceMs, overflow) = configuration.minSilenceMs.addingReportingOverflow(
            configuration.hangoverMs
        )
        let silenceSeconds = min(
            maxSeconds,
            Double(overflow ? UInt64.max : combinedSilenceMs) / 1_000
        )
        let speechSeconds = min(maxSeconds, Double(configuration.minSpeechMs) / 1_000)
        let paddingSeconds = min(
            0.1,
            speechSeconds,
            Double(configuration.preRollMs) / 1_000
        )
        self.segmentation = VadSegmentationConfig(
            minSpeechDuration: speechSeconds,
            minSilenceDuration: silenceSeconds,
            maxSpeechDuration: maxSeconds,
            speechPadding: paddingSeconds,
            silenceThresholdForSplit: max(0, min(1, configuration.threshold - 0.15)),
            negativeThreshold: max(0.01, configuration.threshold - 0.15),
            negativeThresholdOffset: 0.15
        )
    }

    mutating func append(_ data: Data) async throws -> [AudioEngineStreamEvent] {
        pendingSamples.append(contentsOf: try normalizer.append(data))
        return try await drainFullChunks()
    }

    mutating func finish() async throws -> [AudioEngineStreamEvent] {
        pendingSamples.append(contentsOf: normalizer.finish())
        var events = try await drainFullChunks()
        if !pendingSamples.isEmpty {
            let tail = pendingSamples
            pendingSamples.removeAll(keepingCapacity: false)
            events.append(contentsOf: try await process(tail))
        }
        events.append(contentsOf: filter.finish(processedSamples: processedSamples))
        return events
    }

    private mutating func drainFullChunks() async throws -> [AudioEngineStreamEvent] {
        var events: [AudioEngineStreamEvent] = []
        while pendingSamples.count >= VadManager.chunkSize {
            let chunk = Array(pendingSamples.prefix(VadManager.chunkSize))
            pendingSamples.removeFirst(VadManager.chunkSize)
            events.append(contentsOf: try await process(chunk))
        }
        return events
    }

    private mutating func process(_ chunk: [Float]) async throws -> [AudioEngineStreamEvent] {
        let result = try await manager.processStreamingChunk(
            chunk,
            state: streamState,
            config: segmentation
        )
        streamState = result.state
        processedSamples += chunk.count
        let rawStart = result.event?.isStart == true ? result.event?.sampleIndex : nil
        let rawEnd = result.event?.isEnd == true ? result.event?.sampleIndex : nil
        let filtered = filter.consume(
            probability: result.probability,
            processedSamples: processedSamples,
            rawStartSample: rawStart,
            rawEndSample: rawEnd
        )
        if filtered.forceReset {
            streamState = await manager.makeStreamState()
        }
        return filtered.events
    }
}
