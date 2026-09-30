import Foundation
import Hummingbird
import HummingbirdWebSocket
import HTTPTypes
import MagicianMacAudioEngineCore

private let protocolHeader = HTTPField.Name("x-magician-audio-protocol")!
private let audioModelHeader = HTTPField.Name("x-magician-audio-model")!
private let audioLanguageHeader = HTTPField.Name("x-magician-audio-language")!
private let audioVoiceHeader = HTTPField.Name("x-magician-audio-voice")!
private let audioFormatHeader = HTTPField.Name("x-magician-audio-format")!
private let audioProcessingMsHeader = HTTPField.Name("x-magician-audio-processing-ms")!

func makeApplication(runtime: EngineRuntime, token: String, port: Int) -> some ApplicationProtocol {
    let router = Router(context: BasicWebSocketRequestContext.self)

    router.get("/health") { request, _ -> HealthResponse in
        try authorize(request, token: token)
        return await runtime.health()
    }
    router.get("/capabilities") { request, _ -> CapabilitiesResponse in
        try authorize(request, token: token)
        return await runtime.capabilities()
    }
    router.get("/models") { request, _ -> [ModelStateResponse] in
        try authorize(request, token: token)
        return await runtime.listModels()
    }
    router.post("/models/:id/load") { request, context -> ModelStateResponse in
        try authorize(request, token: token)
        let id = try context.parameters.require("id")
        do { return try await runtime.loadModel(id) }
        catch { throw HTTPError(.badRequest, message: error.localizedDescription) }
    }
    router.post("/models/:id/unload") { request, context -> ModelStateResponse in
        try authorize(request, token: token)
        let id = try context.parameters.require("id")
        do { return try await runtime.unloadModel(id) }
        catch { throw HTTPError(.conflict, message: error.localizedDescription) }
    }
    router.post("/v1/audio/transcriptions") { request, context -> TranscriptionResponse in
        try authorize(request, token: token)
        guard let modelId = request.headers[audioModelHeader]?.trimmingCharacters(in: .whitespaces),
            !modelId.isEmpty
        else {
            throw HTTPError(.badRequest, message: "x-magician-audio-model is required")
        }
        let contentType = request.headers[.contentType] ?? "application/octet-stream"
        let body: Data
        do {
            let buffer = try await request.body.collect(upTo: runtime.configuration.maxRequestBytes)
            body = Data(buffer.readableBytesView)
        } catch {
            throw HTTPError(.contentTooLarge, message: "recording exceeds the configured request limit")
        }
        guard !body.isEmpty else {
            throw HTTPError(.badRequest, message: "recording STT requires non-empty audio")
        }
        do {
            let samples = try await RecordingAudioDecoder.decode(body, contentType: contentType)
            return try await runtime.transcribeRecording(
                modelId,
                audioSamples: samples,
                language: request.headers[audioLanguageHeader]
            )
        } catch let error as RecordingSttError {
            throw HTTPError(.badRequest, message: error.localizedDescription)
        } catch let error as RuntimeError {
            throw HTTPError(.serviceUnavailable, message: error.localizedDescription)
        } catch {
            context.logger.warning("FluidAudio recording transcription failed: \(error)")
            throw HTTPError(.serviceUnavailable, message: error.localizedDescription)
        }
    }
    router.post("/v1/audio/speech") { request, context -> Response in
        try authorize(request, token: token)
        guard let modelId = request.headers[audioModelHeader]?.trimmingCharacters(in: .whitespaces),
            !modelId.isEmpty
        else {
            throw HTTPError(.badRequest, message: "x-magician-audio-model is required")
        }
        let body: Data
        do {
            let buffer = try await request.body.collect(upTo: runtime.configuration.maxRequestBytes)
            body = Data(buffer.readableBytesView)
        } catch {
            throw HTTPError(.contentTooLarge, message: "speech request exceeds the configured request limit")
        }
        let synthesisRequest: SpeechSynthesisRequest
        do {
            synthesisRequest = try JSONDecoder().decode(SpeechSynthesisRequest.self, from: body)
        } catch {
            throw HTTPError(.badRequest, message: "invalid speech synthesis request")
        }
        do {
            let result = try await runtime.synthesizeSpeech(modelId, request: synthesisRequest)
            var headers = HTTPFields()
            headers[.contentType] = "audio/wav"
            headers[audioModelHeader] = result.modelId
            headers[audioVoiceHeader] = result.voice
            headers[audioFormatHeader] = result.format
            headers[audioProcessingMsHeader] = String(result.processingDurationMs)
            var buffer = ByteBuffer()
            buffer.writeBytes(result.audio)
            return Response(status: .ok, headers: headers, body: .init(byteBuffer: buffer))
        } catch let error as RuntimeError {
            switch error {
            case .invalidRequest:
                throw HTTPError(.badRequest, message: error.localizedDescription)
            case .modelBusy, .sessionLimit:
                throw HTTPError(.conflict, message: error.localizedDescription)
            default:
                throw HTTPError(.serviceUnavailable, message: error.localizedDescription)
            }
        } catch {
            context.logger.warning("FluidAudio speech synthesis failed: \(error)")
            throw HTTPError(.serviceUnavailable, message: error.localizedDescription)
        }
    }

    router.ws("/v1/audio/stream") { request, _ in
        guard isAuthorized(request, token: token), hasCurrentProtocol(request) else {
            return .dontUpgrade
        }
        return .upgrade()
    } onUpgrade: { inbound, outbound, _ in
        await handleStream(inbound: inbound, outbound: outbound, runtime: runtime)
    }

    return Application(
        router: router,
        server: .http1WebSocketUpgrade(
            webSocketRouter: router,
            configuration: .init(ws: .init(maxFrameSize: runtime.configuration.maxFrameBytes))
        ),
        configuration: .init(
            address: .hostname("127.0.0.1", port: port),
            serverName: "magician-macos-audio-engine"
        )
    )
}

private func handleStream(
    inbound: WebSocketInboundStream,
    outbound: WebSocketOutboundWriter,
    runtime: EngineRuntime
) async {
    var modelId: String?
    var stage: ActiveStreamStage?
    var processor: ActiveStreamProcessor?
    // Ensures the model's streaming slot is released exactly once — eagerly on a
    // clean `stop` (before we ack `.finished`) or as a fallback on socket-close /
    // error exits below.
    var slotReleased = false
    do {
        streamLoop: for try await message in inbound.messages(maxSize: runtime.configuration.maxFrameBytes) {
            switch message {
            case .text(let text):
                guard let data = text.data(using: .utf8),
                    let controlType = try? JSONDecoder().decode(ControlType.self, from: data)
                else {
                    try await send(.error(code: "invalid_control", message: "invalid JSON control"), to: outbound)
                    break streamLoop
                }
                if controlType.type == "start" {
                    guard processor == nil else {
                        try await send(.error(code: "duplicate_start", message: "stream is already started"), to: outbound)
                        break streamLoop
                    }
                    let start = try JSONDecoder().decode(StreamStartControl.self, from: data)
                    guard start.protocolVersion == audioEngineProtocolVersion else {
                        try await send(.error(code: "unsupported_protocol", message: "unsupported stream stage or version"), to: outbound)
                        break streamLoop
                    }
                    switch start.stage {
                    case "vad":
                        let manager = try await runtime.beginVadSession(start.modelId)
                        modelId = start.modelId
                        stage = .vad
                        processor = .vad(try await VadStreamProcessor(
                            manager: manager,
                            format: start.format,
                            configuration: start.config.vad()
                        ))
                    case "streaming_stt":
                        let manager = try await runtime.beginStreamingSttSession(start.modelId)
                        modelId = start.modelId
                        stage = .streamingStt
                        processor = .streamingStt(try await StreamingSttStreamProcessor(
                            manager: manager,
                            format: start.format,
                            configuration: start.config.streamingStt()
                        ))
                    case "diarization":
                        let models = try await runtime.beginDiarizationSession(start.modelId)
                        modelId = start.modelId
                        stage = .diarization
                        processor = .diarization(try DiarizationStreamProcessor(
                            models: models,
                            format: start.format,
                            configuration: start.config.diarization()
                        ))
                    default:
                        try await send(.error(code: "unsupported_stage", message: "unsupported stream stage"), to: outbound)
                        break streamLoop
                    }
                    try await send(.ready, to: outbound)
                } else if controlType.type == "stop" {
                    guard var active = processor else {
                        try await send(.error(code: "not_started", message: "stream has not started"), to: outbound)
                        break streamLoop
                    }
                    for event in try await active.finish() { try await send(event, to: outbound) }
                    // Release the model's streaming slot BEFORE acking `.finished`.
                    // The hands-free cascade reopens a fresh STT stream the instant it
                    // sees the finish; if the slot is still counted here, that reopen
                    // hits `guard record.activeSessions == 0` and fails ("model has
                    // active session"), so the call goes deaf after the first utterance.
                    if let modelId, let stage, !slotReleased {
                        slotReleased = true
                        await releaseStreamSlot(runtime, modelId, stage)
                    }
                    try await send(.finished, to: outbound)
                    break streamLoop
                } else {
                    try await send(.error(code: "unknown_control", message: "unknown control type"), to: outbound)
                    break streamLoop
                }
            case .binary(let buffer):
                guard var active = processor else {
                    try await send(.error(code: "not_started", message: "start control is required before PCM"), to: outbound)
                    break streamLoop
                }
                let data = Data(buffer.readableBytesView)
                for event in try await active.append(data) { try await send(event, to: outbound) }
                processor = active
            }
        }
    } catch {
        try? await send(.error(code: "stream_failed", message: error.localizedDescription), to: outbound)
    }
    if let modelId, let stage, !slotReleased {
        await releaseStreamSlot(runtime, modelId, stage)
    }
}

/// Release a streaming model's slot for the given stage. Extracted so the
/// clean-stop path (which releases *before* acking `.finished`) and the
/// fallback socket-close / error exit share one implementation.
private func releaseStreamSlot(_ runtime: EngineRuntime, _ modelId: String, _ stage: ActiveStreamStage) async {
    switch stage {
    case .vad: await runtime.endVadSession(modelId)
    case .streamingStt: await runtime.endStreamingSttSession(modelId)
    case .diarization: await runtime.endDiarizationSession(modelId)
    }
}

private enum ActiveStreamStage {
    case vad
    case streamingStt
    case diarization
}

private enum ActiveStreamProcessor {
    case vad(VadStreamProcessor)
    case streamingStt(StreamingSttStreamProcessor)
    case diarization(DiarizationStreamProcessor)

    mutating func append(_ data: Data) async throws -> [AudioEngineStreamEvent] {
        switch self {
        case .vad(var processor):
            let events = try await processor.append(data)
            self = .vad(processor)
            return events
        case .streamingStt(var processor):
            let events = try await processor.append(data)
            self = .streamingStt(processor)
            return events
        case .diarization(var processor):
            let events = try processor.append(data)
            self = .diarization(processor)
            return events
        }
    }

    mutating func finish() async throws -> [AudioEngineStreamEvent] {
        switch self {
        case .vad(var processor): return try await processor.finish()
        case .streamingStt(var processor): return try await processor.finish()
        case .diarization(var processor): return try processor.finish()
        }
    }
}

private struct ControlType: Decodable { let type: String }

private func send(_ event: AudioEngineStreamEvent, to outbound: WebSocketOutboundWriter) async throws {
    let data = try JSONEncoder().encode(event)
    guard let text = String(data: data, encoding: .utf8) else {
        throw HTTPError(.internalServerError, message: "failed to encode stream event")
    }
    try await outbound.write(.text(text))
}

private func authorize(_ request: Request, token: String) throws {
    guard isAuthorized(request, token: token) else { throw HTTPError(.unauthorized) }
    guard hasCurrentProtocol(request) else {
        throw HTTPError(.upgradeRequired, message: "unsupported audio protocol version")
    }
}

private func isAuthorized(_ request: Request, token: String) -> Bool {
    request.headers[.authorization] == "Bearer \(token)"
}

private func hasCurrentProtocol(_ request: Request) -> Bool {
    request.headers[protocolHeader] == String(audioEngineProtocolVersion)
}
