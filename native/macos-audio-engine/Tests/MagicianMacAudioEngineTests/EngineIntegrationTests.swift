import Foundation
import Hummingbird
import HummingbirdTesting
import HummingbirdWSClient
import HummingbirdWSTesting
import HTTPTypes
@testable import MagicianMacAudioEngine
@testable import MagicianMacAudioEngineCore
import XCTest

final class EngineIntegrationTests: XCTestCase {
    private let token = String(repeating: "a", count: 64)
    private let protocolHeader = HTTPField.Name("x-magician-audio-protocol")!

    func testHealthAndModelInventoryRequireAuthenticationAndVersion() async throws {
        let runtime = EngineRuntime(configuration: makeConfiguration())
        let app = makeApplication(runtime: runtime, token: token, port: 0)

        try await app.test(.router) { client in
            try await client.execute(uri: "/health", method: .get) { response in
                XCTAssertEqual(response.status, .unauthorized)
            }
            try await client.execute(
                uri: "/health",
                method: .get,
                headers: authorizedHeaders()
            ) { response in
                XCTAssertEqual(response.status, .ok)
                let health = try JSONDecoder().decode(HealthResponse.self, from: response.body)
                XCTAssertEqual(health.status, "ok")
                XCTAssertEqual(health.protocolVersion, audioEngineProtocolVersion)
            }
            try await client.execute(
                uri: "/models",
                method: .get,
                headers: authorizedHeaders()
            ) { response in
                XCTAssertEqual(response.status, .ok)
                let models = try JSONDecoder().decode([ModelStateResponse].self, from: response.body)
                XCTAssertEqual(models.count, 1)
                XCTAssertEqual(models[0].id, "fluid-silero-v6")
                XCTAssertFalse(models[0].resident)
            }
        }
    }

    func testWebSocketRequiresAuthAndReturnsVersionedProtocolErrors() async throws {
        let runtime = EngineRuntime(configuration: makeConfiguration())
        let app = makeApplication(runtime: runtime, token: token, port: 0)

        try await app.test(.live) { client in
            do {
                try await client.ws("/v1/audio/stream") { _, _, _ in }
                XCTFail("unauthenticated WebSocket unexpectedly upgraded")
            } catch {}

            let configuration = WebSocketClientConfiguration(
                additionalHeaders: authorizedHeaders()
            )
            try await client.ws("/v1/audio/stream", configuration: configuration) {
                inbound, outbound, _ in
                var iterator = inbound.messages(maxSize: 4_096).makeAsyncIterator()
                try await outbound.write(.text("{}"))
                guard case .text(let payload) = try await iterator.next() else {
                    return XCTFail("sidecar did not return a protocol error")
                }
                XCTAssertTrue(payload.contains("invalid_control"))
            }
        }
    }

    func testOfflinePolicyRejectsMissingCacheWithoutLoading() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let runtime = EngineRuntime(configuration: makeConfiguration(cache: root.path))

        do {
            _ = try await runtime.loadModel("fluid-silero-v6")
            XCTFail("missing offline model unexpectedly loaded")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("not cached"))
        }
        let models = await runtime.listModels()
        XCTAssertEqual(models.first?.state, "unloaded")
        XCTAssertFalse(models.first?.resident ?? true)
    }

    func testConfigurationAcceptsPinnedQwenRecordingModel() throws {
        let config = makeConfiguration(models: [vadDefinition(), qwenDefinition()])
        let json = String(decoding: try JSONEncoder().encode(config), as: UTF8.self)
        let loaded = try EngineConfiguration.load(environment: [
            "MAGICIAN_AUDIO_ENGINE_TOKEN": token,
            "MAGICIAN_AUDIO_ENGINE_CONFIG_JSON": json,
            "MAGICIAN_AUDIO_ENGINE_ENDPOINT": "http://127.0.0.1:3029",
        ])
        XCTAssertEqual(loaded.config.models.count, 2)
        XCTAssertEqual(loaded.config.models[1].adapter, RecordingSttModelFactory.adapter)
    }

    func testConfigurationAcceptsConfiguredKokoroTtsModel() throws {
        let config = makeConfiguration(models: [kokoroDefinition()])
        let json = String(decoding: try JSONEncoder().encode(config), as: UTF8.self)
        let loaded = try EngineConfiguration.load(environment: [
            "MAGICIAN_AUDIO_ENGINE_TOKEN": token,
            "MAGICIAN_AUDIO_ENGINE_CONFIG_JSON": json,
            "MAGICIAN_AUDIO_ENGINE_ENDPOINT": "http://127.0.0.1:3029",
        ])
        XCTAssertEqual(loaded.config.models[0].adapter, "fluid_audio_kokoro_tts")
        XCTAssertEqual(loaded.config.models[0].voice, "af_heart")
        XCTAssertEqual(loaded.config.models[0].formats, ["wav"])
    }

    func testKokoroAuxiliaryInventoryIncludesG2PBundle() {
        let paths = Set(EngineRuntime.kokoroAuxiliaryAssets(
            voice: "af_heart",
            requireLexicon: true
        ).map(\.relativePath))
        XCTAssertEqual(paths, [
            "voices/af_heart.json",
            "vocab_index.json",
            "g2p_vocab.json",
            "G2PEncoder.mlmodelc",
            "G2PDecoder.mlmodelc",
            "us_lexicon_cache.json",
        ])
        XCTAssertFalse(EngineRuntime.kokoroAuxiliaryAssets(
            voice: nil,
            requireLexicon: true
        ).contains { $0.relativePath.hasPrefix("voices/") })
    }

    func testKokoroAuxiliaryCopyRestoresAndReportsG2PBundle() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let portable = root.appendingPathComponent("portable", isDirectory: true)
        let sdk = root.appendingPathComponent("sdk", isDirectory: true)
        let assets = EngineRuntime.kokoroAuxiliaryAssets(
            voice: "af_heart",
            requireLexicon: true
        )
        for asset in assets where asset.relativePath != "g2p_vocab.json" {
            let url = portable.appendingPathComponent(asset.relativePath)
            if asset.relativePath.hasSuffix(".mlmodelc") {
                try FileManager.default.createDirectory(
                    at: url,
                    withIntermediateDirectories: true
                )
                try Data(asset.relativePath.utf8).write(
                    to: url.appendingPathComponent("marker.bin")
                )
            } else {
                try FileManager.default.createDirectory(
                    at: url.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )
                try Data(asset.relativePath.utf8).write(to: url)
            }
        }

        var missing = try EngineRuntime.copyKokoroAuxiliaryAssets(
            from: portable,
            to: sdk,
            voice: "af_heart",
            requireLexicon: true
        )
        XCTAssertEqual(missing, ["G2P vocabulary"])

        let g2p = portable.appendingPathComponent("g2p_vocab.json")
        try Data("g2p".utf8).write(to: g2p)
        missing = try EngineRuntime.copyKokoroAuxiliaryAssets(
            from: portable,
            to: sdk,
            voice: "af_heart",
            requireLexicon: true
        )
        XCTAssertTrue(missing.isEmpty)
        XCTAssertEqual(
            try Data(contentsOf: sdk.appendingPathComponent("g2p_vocab.json")),
            Data("g2p".utf8)
        )
        XCTAssertTrue(FileManager.default.fileExists(
            atPath: sdk.appendingPathComponent("G2PEncoder.mlmodelc/marker.bin").path
        ))
        XCTAssertTrue(FileManager.default.fileExists(
            atPath: sdk.appendingPathComponent("G2PDecoder.mlmodelc/marker.bin").path
        ))
    }

    func testOfflinePolicyRejectsMissingQwenCacheWithoutLoading() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let runtime = EngineRuntime(configuration: makeConfiguration(
            cache: root.path,
            models: [qwenDefinition()]
        ))

        do {
            _ = try await runtime.loadModel("fluid-qwen3-asr-f32")
            XCTFail("missing offline Qwen model unexpectedly loaded")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("not cached"))
        }
    }

    func testRecordingEndpointRequiresModelAndAudio() async throws {
        let runtime = EngineRuntime(configuration: makeConfiguration())
        let app = makeApplication(runtime: runtime, token: token, port: 0)

        try await app.test(.router) { client in
            try await client.execute(
                uri: "/v1/audio/transcriptions",
                method: .post,
                headers: authorizedHeaders()
            ) { response in
                XCTAssertEqual(response.status, .badRequest)
            }

            var headers = authorizedHeaders()
            headers[HTTPField.Name("x-magician-audio-model")!] = "fluid-qwen3-asr-f32"
            try await client.execute(
                uri: "/v1/audio/transcriptions",
                method: .post,
                headers: headers
            ) { response in
                XCTAssertEqual(response.status, .badRequest)
            }
        }
    }

    func testSpeechEndpointRequiresConfiguredModelAndTypedBody() async throws {
        let runtime = EngineRuntime(configuration: makeConfiguration())
        let app = makeApplication(runtime: runtime, token: token, port: 0)

        try await app.test(.router) { client in
            try await client.execute(
                uri: "/v1/audio/speech",
                method: .post,
                headers: authorizedHeaders()
            ) { response in
                XCTAssertEqual(response.status, .badRequest)
            }

            var headers = authorizedHeaders()
            headers[HTTPField.Name("x-magician-audio-model")!] = "fluid-kokoro-en"
            try await client.execute(
                uri: "/v1/audio/speech",
                method: .post,
                headers: headers,
                body: ByteBuffer(string: "{}")
            ) { response in
                XCTAssertEqual(response.status, .badRequest)
            }
        }
    }

    func testRecordingAudioDecoderNormalizesWavToSixteenKilohertzMono() async throws {
        let samples = try await RecordingAudioDecoder.decode(
            silentPcm16Wav(sampleRate: 8_000, sampleCount: 800),
            contentType: "audio/wav"
        )
        XCTAssertTrue((1_598...1_602).contains(samples.count))
        XCTAssertTrue(samples.allSatisfy { $0.isFinite && abs($0) < 0.000_001 })
    }

    func testIdleProcessPolicyExitsWhenNothingIsResident() async {
        let runtime = EngineRuntime(
            configuration: makeConfiguration(processIdleSecs: 0)
        )
        let shouldExit = await runtime.performIdleSweep()
        XCTAssertTrue(shouldExit)
    }

    func testConfigurationRejectsUnsafeSidecarEndpoints() throws {
        let config = makeConfiguration()
        let data = try JSONEncoder().encode(config)
        let json = String(decoding: data, as: UTF8.self)
        for endpoint in [
            "http://example.com:3029",
            "https://127.0.0.1:3029",
            "http://user:pass@127.0.0.1:3029",
            "http://127.0.0.1:3029/health",
            "http://127.0.0.1:3029?debug=1",
        ] {
            XCTAssertThrowsError(try EngineConfiguration.load(environment: [
                "MAGICIAN_AUDIO_ENGINE_TOKEN": token,
                "MAGICIAN_AUDIO_ENGINE_CONFIG_JSON": json,
                "MAGICIAN_AUDIO_ENGINE_ENDPOINT": endpoint,
            ]), "unexpectedly accepted \(endpoint)")
        }
    }

    func testHardwareVadWhenExplicitlyEnabled() async throws {
        guard ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_HARDWARE_TEST"] == "1" else {
            throw XCTSkip("set MAGICIAN_FLUID_AUDIO_HARDWARE_TEST=1 to run CoreML inference")
        }
        let cache = ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_TEST_MODEL_CACHE"]
            ?? FileManager.default.temporaryDirectory
                .appendingPathComponent("magician-fluidaudio-hardware", isDirectory: true).path
        let configuration = makeConfiguration(
            cache: cache,
            downloadPolicy: .onDemand,
            offline: false
        )
        let runtime = EngineRuntime(configuration: configuration)
        let manager = try await runtime.beginVadSession("fluid-silero-v6")
        var processor = try await VadStreamProcessor(
            manager: manager,
            format: .init(sampleRateHz: 16_000, channels: 1, sampleFormat: .pcmS16Le),
            configuration: .init(
                threshold: 0.65,
                minSpeechMs: 250,
                minSilenceMs: 500,
                preRollMs: 400,
                hangoverMs: 600,
                maxUtteranceMs: 120_000,
                gateOnly: true
            )
        )
        var events = try await processor.append(Data(repeating: 0, count: 4_096 * 2))
        events.append(contentsOf: try await processor.finish())
        await runtime.endVadSession("fluid-silero-v6")
        XCTAssertTrue(events.contains { event in
            if case .probability(let value, _) = event { return value.isFinite }
            return false
        })
    }

    func testHardwareQwenRecordingWhenExplicitlyEnabled() async throws {
        guard ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_STT_HARDWARE_TEST"] == "1"
        else {
            throw XCTSkip("set MAGICIAN_FLUID_AUDIO_STT_HARDWARE_TEST=1 to run Qwen3 ASR inference")
        }
        guard let fixture = ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_STT_FIXTURE"]
        else {
            return XCTFail("MAGICIAN_FLUID_AUDIO_STT_FIXTURE must name a WAV fixture")
        }
        let cache = ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_TEST_MODEL_CACHE"]
            ?? FileManager.default.temporaryDirectory
                .appendingPathComponent("magician-fluidaudio-hardware", isDirectory: true).path
        let runtime = EngineRuntime(configuration: makeConfiguration(
            cache: cache,
            downloadPolicy: .onDemand,
            offline: false,
            models: [qwenDefinition()]
        ))
        let samples = try await RecordingAudioDecoder.decode(
            Data(contentsOf: URL(fileURLWithPath: fixture)),
            contentType: "audio/wav"
        )
        let response = try await runtime.transcribeRecording(
            "fluid-qwen3-asr-f32",
            audioSamples: samples,
            language: "en"
        )
        XCTAssertFalse(response.transcript.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        XCTAssertEqual(response.modelId, "fluid-qwen3-asr-f32")
        XCTAssertGreaterThan(response.processingDurationMs, 0)
    }

    func testHardwareKokoroTtsWhenExplicitlyEnabled() async throws {
        guard ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_TTS_HARDWARE_TEST"] == "1"
        else {
            throw XCTSkip("set MAGICIAN_FLUID_AUDIO_TTS_HARDWARE_TEST=1 to run Kokoro TTS inference")
        }
        let cache = ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_TEST_MODEL_CACHE"]
            ?? FileManager.default.temporaryDirectory
                .appendingPathComponent("magician-fluidaudio-hardware", isDirectory: true).path
        let runtime = EngineRuntime(configuration: makeConfiguration(
            cache: cache,
            downloadPolicy: .onDemand,
            offline: false,
            models: [kokoroDefinition()]
        ))
        let response = try await runtime.synthesizeSpeech(
            "fluid-kokoro-en",
            request: SpeechSynthesisRequest(
                input: "Magician local speech is ready.",
                voice: "af_heart",
                responseFormat: "wav",
                speed: 1.0
            )
        )
        XCTAssertEqual(String(decoding: response.audio.prefix(4), as: UTF8.self), "RIFF")
        XCTAssertEqual(response.voice, "af_heart")
        XCTAssertGreaterThan(response.processingDurationMs, 0)
    }

    func testHardwareZOfflineCachedModelsWhenExplicitlyEnabled() async throws {
        guard ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_OFFLINE_HARDWARE_TEST"] == "1"
        else {
            throw XCTSkip("set MAGICIAN_FLUID_AUDIO_OFFLINE_HARDWARE_TEST=1 to verify cached offline inference")
        }
        guard let cache = ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_TEST_MODEL_CACHE"]
        else {
            return XCTFail("MAGICIAN_FLUID_AUDIO_TEST_MODEL_CACHE must name a prepared cache")
        }
        guard let fixture = ProcessInfo.processInfo.environment["MAGICIAN_FLUID_AUDIO_STT_FIXTURE"]
        else {
            return XCTFail("MAGICIAN_FLUID_AUDIO_STT_FIXTURE must name a WAV fixture")
        }

        let vad = EngineRuntime(configuration: makeConfiguration(cache: cache, models: [vadDefinition()]))
        let vadState = try await vad.loadModel("fluid-silero-v6")
        XCTAssertTrue(vadState.resident)

        let recording = EngineRuntime(configuration: makeConfiguration(
            cache: cache,
            models: [qwenDefinition()]
        ))
        let samples = try await RecordingAudioDecoder.decode(
            Data(contentsOf: URL(fileURLWithPath: fixture)),
            contentType: "audio/wav"
        )
        let transcript = try await recording.transcribeRecording(
            "fluid-qwen3-asr-f32",
            audioSamples: samples,
            language: "en"
        )
        XCTAssertFalse(transcript.transcript.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

        let tts = EngineRuntime(configuration: makeConfiguration(cache: cache, models: [kokoroDefinition()]))
        let speech = try await tts.synthesizeSpeech(
            "fluid-kokoro-en",
            request: SpeechSynthesisRequest(
                input: "Magician offline speech is ready.",
                voice: "af_heart",
                responseFormat: "wav",
                speed: 1.0
            )
        )
        XCTAssertEqual(String(decoding: speech.audio.prefix(4), as: UTF8.self), "RIFF")
    }

    private func authorizedHeaders() -> HTTPFields {
        var headers: HTTPFields = [:]
        headers[.authorization] = "Bearer \(token)"
        headers[protocolHeader] = String(audioEngineProtocolVersion)
        return headers
    }

    private func makeConfiguration(
        cache: String = "/tmp/magician-fluidaudio-tests",
        downloadPolicy: DownloadPolicy = .disabled,
        offline: Bool = true,
        processIdleSecs: UInt64 = 900,
        models: [ModelDefinition]? = nil
    ) -> EngineConfiguration {
        EngineConfiguration(
            protocolVersion: audioEngineProtocolVersion,
            modelCacheDir: cache,
            downloadPolicy: downloadPolicy,
            registryUrl: nil,
            offline: offline,
            processIdleSecs: processIdleSecs,
            maxResidentModels: 1,
            maxStreamingSessions: 2,
            maxRequestBytes: 25 * 1_024 * 1_024,
            maxFrameBytes: 1_024 * 1_024,
            prewarm: [],
            models: models ?? [vadDefinition()]
        )
    }

    private func vadDefinition() -> ModelDefinition {
        ModelDefinition(
            id: "fluid-silero-v6",
            adapter: "fluid_audio_vad",
            repository: "FluidInference/silero-vad-coreml",
            variant: "silero-vad-unified-256ms-v6.0.0.mlmodelc",
            revision: "main",
            sha256: nil,
            idleSecs: 300
        )
    }

    private func qwenDefinition() -> ModelDefinition {
        ModelDefinition(
            id: "fluid-qwen3-asr-f32",
            adapter: RecordingSttModelFactory.adapter,
            repository: RecordingSttModelFactory.repository,
            variant: "f32",
            revision: "main",
            sha256: nil,
            idleSecs: 300
        )
    }

    private func kokoroDefinition() -> ModelDefinition {
        ModelDefinition(
            id: "fluid-kokoro-en",
            adapter: "fluid_audio_kokoro_tts",
            repository: "FluidInference/kokoro-82m-coreml",
            variant: "15s",
            revision: "main",
            sha256: nil,
            idleSecs: 300,
            voice: "af_heart",
            voices: ["af_heart", "af_kore", "am_michael"],
            formats: ["wav"]
        )
    }

    private func silentPcm16Wav(sampleRate: Int, sampleCount: Int) -> Data {
        let dataBytes = sampleCount * 2
        var data = Data()
        func appendAscii(_ value: String) { data.append(contentsOf: value.utf8) }
        func append16(_ value: UInt16) {
            var value = value.littleEndian
            withUnsafeBytes(of: &value) { data.append(contentsOf: $0) }
        }
        func append32(_ value: UInt32) {
            var value = value.littleEndian
            withUnsafeBytes(of: &value) { data.append(contentsOf: $0) }
        }
        appendAscii("RIFF")
        append32(UInt32(36 + dataBytes))
        appendAscii("WAVEfmt ")
        append32(16)
        append16(1)
        append16(1)
        append32(UInt32(sampleRate))
        append32(UInt32(sampleRate * 2))
        append16(2)
        append16(16)
        appendAscii("data")
        append32(UInt32(dataBytes))
        data.append(Data(repeating: 0, count: dataBytes))
        return data
    }
}
