import FluidAudio
import Foundation
import MagicianMacAudioEngineCore

private enum StreamingSttProcessorError: Error, LocalizedError {
    case invalidConfiguration(String)

    var errorDescription: String? {
        switch self { case .invalidConfiguration(let reason): reason }
    }
}

private final class StreamingEventCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var events: [AudioEngineStreamEvent] = []
    private var turnId = UUID().uuidString
    private var turnStartMs: UInt64 = 0
    private var cursorMs: UInt64 = 0
    private var lastPartial = ""
    private let language: String?

    init(language: String?) {
        self.language = language
    }

    func updateCursor(_ value: UInt64) {
        lock.lock()
        cursorMs = value
        lock.unlock()
    }

    func partial(_ rawText: String) {
        let text = rawText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }
        guard text != lastPartial else { return }
        if lastPartial.isEmpty { turnStartMs = cursorMs }
        lastPartial = text
        events.append(.transcriptPartial(text: text, turnId: turnId, startMs: turnStartMs))
    }

    func final(_ rawText: String) {
        let text = rawText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }
        events.append(.transcriptFinal(
            text: text,
            turnId: turnId,
            language: language,
            startMs: turnStartMs
        ))
        turnId = UUID().uuidString
        turnStartMs = cursorMs
        lastPartial = ""
    }

    func drain() -> [AudioEngineStreamEvent] {
        lock.lock()
        defer { lock.unlock() }
        let output = events
        events.removeAll(keepingCapacity: true)
        return output
    }
}

struct StreamingSttStreamProcessor {
    private let manager: StreamingEouAsrManager
    private let collector: StreamingEventCollector
    private var normalizer: PCMNormalizer
    private var processedSamples: UInt64 = 0

    init(
        manager: StreamingEouAsrManager,
        format: StreamAudioFormat,
        configuration: StreamingSttSessionConfiguration
    ) async throws {
        guard (160...10_000).contains(configuration.eouDebounceMs) else {
            throw StreamingSttProcessorError.invalidConfiguration(
                "streaming STT EOU debounce must be between 160 ms and 10 seconds"
            )
        }
        self.manager = manager
        self.normalizer = try PCMNormalizer(format: format)
        self.collector = StreamingEventCollector(language: configuration.language)
        await manager.setPartialCallback { [collector] text in collector.partial(text) }
        await manager.setEouCallback { [collector] text in collector.final(text) }
        await manager.reset()
    }

    mutating func append(_ data: Data) async throws -> [AudioEngineStreamEvent] {
        try await process(try normalizer.append(data))
    }

    mutating func finish() async throws -> [AudioEngineStreamEvent] {
        var events = try await process(normalizer.finish())
        collector.updateCursor(milliseconds(processedSamples))
        collector.final(try await manager.finish())
        events.append(contentsOf: collector.drain())
        return events
    }

    private mutating func process(_ samples: [Float]) async throws -> [AudioEngineStreamEvent] {
        guard !samples.isEmpty else { return collector.drain() }
        collector.updateCursor(milliseconds(processedSamples))
        processedSamples += UInt64(samples.count)
        _ = try await manager.process(audioBuffer: makeMono16KhzBuffer(samples))
        var events = collector.drain()
        if await manager.eouDetected {
            await manager.reset()
            collector.updateCursor(milliseconds(processedSamples))
            events.append(contentsOf: collector.drain())
        }
        return events
    }

    private func milliseconds(_ samples: UInt64) -> UInt64 {
        samples * 1_000 / UInt64(PCMNormalizer.outputSampleRate)
    }
}
