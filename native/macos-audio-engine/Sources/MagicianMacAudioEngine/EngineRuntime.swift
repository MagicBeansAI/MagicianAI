import CryptoKit
import CoreML
import FluidAudio
import Foundation
import Hummingbird
import MagicianMacAudioEngineCore

struct HealthResponse: ResponseCodable, Sendable { let status: String; let protocolVersion: Int
    enum CodingKeys: String, CodingKey { case status; case protocolVersion = "protocol_version" }
}

struct CapabilitiesResponse: ResponseCodable, Sendable {
    let protocolVersion: Int
    let stages: [String]
    let sampleFormats: [String]
    let inputSampleRateMin: Int
    let inputSampleRateMax: Int
    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version", stages
        case sampleFormats = "sample_formats"
        case inputSampleRateMin = "input_sample_rate_min"
        case inputSampleRateMax = "input_sample_rate_max"
    }
}

struct ModelStateResponse: ResponseCodable, Sendable {
    let id: String
    let state: String
    let repository: String
    let resident: Bool
    let activeSessions: Int
    enum CodingKeys: String, CodingKey {
        case id, state, repository, resident
        case activeSessions = "active_sessions"
    }
}

struct TranscriptionResponse: ResponseCodable, Sendable {
    let transcript: String
    let modelId: String
    let model: String
    let variant: String?
    let language: String?
    let audioDurationMs: UInt64
    let processingDurationMs: UInt64
    let confidence: Float?

    enum CodingKeys: String, CodingKey {
        case transcript, model, variant, language, confidence
        case modelId = "model_id"
        case audioDurationMs = "audio_duration_ms"
        case processingDurationMs = "processing_duration_ms"
    }
}

struct SpeechSynthesisResult: Sendable {
    let audio: Data
    let modelId: String
    let model: String
    let voice: String
    let format: String
    let processingDurationMs: UInt64
}

private struct ModelRecord {
    let definition: ModelDefinition
    var runtime: LoadedModelRuntime?
    var loading = false
    var activeSessions = 0
    var lastUsed = ContinuousClock.now
}

private enum LoadedModelRuntime: Sendable {
    case vad(VadManager)
    case recordingStt(any RecordingSttModel)
    case streamingStt(StreamingEouAsrManager)
    case diarization(SortformerRuntimeBox)
    case tts(KokoroRuntimeBox)
}

private final class SortformerRuntimeBox: @unchecked Sendable {
    let models: SortformerModels

    init(models: SortformerModels) {
        self.models = models
    }
}

private actor KokoroRuntimeBox {
    private let manager: KokoroTtsManager
    let variant: ModelNames.TTS.Variant
    let voices: Set<String>
    let formats: Set<String>

    init(definition: ModelDefinition, cacheRoot: URL, variant: ModelNames.TTS.Variant) {
        let defaultVoice = definition.voice ?? TtsConstants.recommendedVoice
        self.manager = KokoroTtsManager(defaultVoice: defaultVoice, directory: cacheRoot)
        self.variant = variant
        self.voices = Set(definition.voices ?? [defaultVoice])
        self.formats = Set(definition.formats ?? ["wav"])
    }

    func initialize(models: TtsModels, defaultVoice: String) async throws {
        try await manager.initialize(models: models, preloadVoices: [defaultVoice])
    }

    func synthesize(text: String, voice: String, speed: Float) async throws -> Data {
        try await manager.synthesize(
            text: text,
            voice: voice,
            voiceSpeed: speed,
            variantPreference: variant
        )
    }

    func supportsVoice(_ voice: String) -> Bool { voices.contains(voice) }

    func supportsFormat(_ format: String) -> Bool { formats.contains(format) }
}

actor EngineRuntime {
    nonisolated let configuration: EngineConfiguration
    private var models: [String: ModelRecord]
    private var loadingTasks: [String: (generation: UInt64, task: Task<LoadedModelRuntime, Error>)] = [:]
    private var nextLoadGeneration: UInt64 = 0
    private var activeSessions = 0
    private var lastActivity = ContinuousClock.now

    init(configuration: EngineConfiguration) {
        self.configuration = configuration
        models = Dictionary(uniqueKeysWithValues: configuration.models.map {
            ($0.id, ModelRecord(definition: $0))
        })
        if let registryUrl = configuration.registryUrl, !registryUrl.isEmpty {
            ModelRegistry.baseURL = registryUrl
        }
    }

    func health() -> HealthResponse {
        HealthResponse(status: "ok", protocolVersion: audioEngineProtocolVersion)
    }

    func capabilities() -> CapabilitiesResponse {
        CapabilitiesResponse(
            protocolVersion: audioEngineProtocolVersion,
            stages: ["vad", "recording_stt", "streaming_stt", "diarization", "tts"],
            sampleFormats: AudioSampleFormat.allCases.map(\.rawValue),
            inputSampleRateMin: 8_000,
            inputSampleRateMax: 192_000
        )
    }

    func listModels() -> [ModelStateResponse] {
        models.values.map(stateResponse).sorted { $0.id < $1.id }
    }

    func loadModel(_ id: String) async throws -> ModelStateResponse {
        guard var record = models[id] else { throw RuntimeError.unknownModel(id) }
        if record.runtime != nil {
            record.lastUsed = .now
            models[id] = record
            lastActivity = .now
            return stateResponse(record)
        }

        let generation: UInt64
        let task: Task<LoadedModelRuntime, Error>
        if let existing = loadingTasks[id] {
            generation = existing.generation
            task = existing.task
        } else {
            try enforceCachePolicy(record.definition)
            try unloadLeastRecentlyUsedIfNeeded(excluding: id)
            let localOnly = configuration.offline || configuration.downloadPolicy == .disabled
            if localOnly { try verifyChecksumIfConfigured(record.definition) }
            let cacheDirectory = configuration.modelCacheDir
            let definition = record.definition
            task = Task.detached(priority: .userInitiated) {
                switch definition.adapter {
                case "fluid_audio_vad":
                    let vadConfig = VadConfig(defaultThreshold: 0.65)
                    if localOnly {
                        let modelConfiguration = MLModelConfiguration()
                        modelConfiguration.computeUnits = vadConfig.computeUnits
                        modelConfiguration.allowLowPrecisionAccumulationOnGPU = true
                        let model = try MLModel(
                            contentsOf: Self.vadModelPath(
                                definition: definition,
                                cacheDirectory: cacheDirectory
                            ),
                            configuration: modelConfiguration
                        )
                        return .vad(VadManager(config: vadConfig, vadModel: model))
                    }
                    return .vad(try await VadManager(
                        config: vadConfig,
                        modelDirectory: URL(fileURLWithPath: cacheDirectory, isDirectory: true)
                    ))
                case RecordingSttModelFactory.adapter:
                    return .recordingStt(try await RecordingSttModelFactory.load(
                        definition: definition,
                        cacheRoot: URL(fileURLWithPath: cacheDirectory, isDirectory: true),
                        localOnly: localOnly
                    ))
                case "fluid_audio_streaming_eou_stt":
                    let chunkSize = try Self.streamingChunkSize(definition.variant)
                    let modelDirectory = Self.streamingSttModelPath(
                        definition: definition,
                        cacheDirectory: cacheDirectory
                    )
                    if !Self.streamingSttModelsExist(at: modelDirectory) {
                        guard !localOnly else { throw RuntimeError.cacheRequired(definition.id) }
                        let repo: Repo = definition.variant == "320ms" ? .parakeetEou320 : .parakeetEou160
                        try await DownloadUtils.downloadRepo(
                            repo,
                            to: URL(fileURLWithPath: cacheDirectory, isDirectory: true)
                        )
                    }
                    let modelConfiguration = MLModelConfiguration()
                    modelConfiguration.computeUnits = .all
                    let manager = StreamingEouAsrManager(
                        configuration: modelConfiguration,
                        chunkSize: chunkSize,
                        eouDebounceMs: 1_280
                    )
                    try await manager.loadModels(modelDir: modelDirectory)
                    return .streamingStt(manager)
                case "fluid_audio_streaming_sortformer":
                    let config = SortformerConfig.default
                    let cacheRoot = URL(fileURLWithPath: cacheDirectory, isDirectory: true)
                    let models: SortformerModels
                    if localOnly {
                        let path = Self.diarizationModelPath(cacheDirectory: cacheDirectory)
                        let modelConfiguration = MLModelConfiguration()
                        modelConfiguration.computeUnits = .all
                        let model = try MLModel(contentsOf: path, configuration: modelConfiguration)
                        models = try SortformerModels(
                            config: config,
                            main: model,
                            compilationDuration: 0
                        )
                    } else {
                        models = try await SortformerModels.loadFromHuggingFace(
                            config: config,
                            cacheDirectory: cacheRoot
                        )
                    }
                    return .diarization(SortformerRuntimeBox(models: models))
                case "fluid_audio_kokoro_tts":
                    let variant = try Self.kokoroVariant(definition.variant)
                    let cacheRoot = URL(fileURLWithPath: cacheDirectory, isDirectory: true)
                    let defaultVoice = definition.voice ?? TtsConstants.recommendedVoice
                    if localOnly {
                        try Self.requireKokoroAuxiliaryAssets(
                            modelID: definition.id,
                            cacheDirectory: cacheDirectory,
                            voice: defaultVoice,
                            requireLexicon: true
                        )
                    }
                    let models: TtsModels
                    if localOnly {
                        let modelConfiguration = MLModelConfiguration()
                        modelConfiguration.computeUnits = .cpuAndGPU
                        let model = try MLModel(
                            contentsOf: Self.kokoroModelPath(
                                cacheDirectory: cacheDirectory,
                                variant: variant
                            ),
                            configuration: modelConfiguration
                        )
                        models = TtsModels(models: [variant: model])
                    } else {
                        models = try await TtsModels.download(
                            variants: [variant],
                            from: definition.repository,
                            directory: cacheRoot
                        )
                        try Self.restoreAvailableKokoroAuxiliaryAssets(
                            cacheDirectory: cacheDirectory,
                            voice: defaultVoice,
                            requireLexicon: true
                        )
                    }
                    let runtime = KokoroRuntimeBox(
                        definition: definition,
                        cacheRoot: cacheRoot,
                        variant: variant
                    )
                    try await runtime.initialize(models: models, defaultVoice: defaultVoice)
                    if !localOnly {
                        try Self.persistKokoroAuxiliaryAssets(
                            modelID: definition.id,
                            cacheDirectory: cacheDirectory,
                            voice: defaultVoice,
                            requireLexicon: true
                        )
                        try Self.requireKokoroAuxiliaryAssets(
                            modelID: definition.id,
                            cacheDirectory: cacheDirectory,
                            voice: defaultVoice,
                            requireLexicon: true
                        )
                    }
                    return .tts(runtime)
                default:
                    throw RuntimeError.unsupportedAdapter(definition.adapter)
                }
            }
            nextLoadGeneration &+= 1
            generation = nextLoadGeneration
            loadingTasks[id] = (generation, task)
            record.loading = true
            models[id] = record
        }

        do {
            let manager = try await task.value
            if loadingTasks[id]?.generation == generation {
                guard var loadedRecord = models[id] else { throw RuntimeError.unknownModel(id) }
                if !configuration.offline && configuration.downloadPolicy != .disabled {
                    try verifyChecksumIfConfigured(loadedRecord.definition)
                }
                loadedRecord.runtime = manager
                loadedRecord.loading = false
                loadedRecord.lastUsed = .now
                models[id] = loadedRecord
                loadingTasks[id] = nil
                lastActivity = .now
            }
            guard let loadedRecord = models[id], loadedRecord.runtime != nil else {
                throw RuntimeError.modelLoad(id, "model load was superseded")
            }
            return stateResponse(loadedRecord)
        } catch {
            if loadingTasks[id]?.generation == generation {
                if var failedRecord = models[id] {
                    failedRecord.loading = false
                    models[id] = failedRecord
                }
                loadingTasks[id] = nil
            }
            throw RuntimeError.modelLoad(id, error.localizedDescription)
        }
    }

    func unloadModel(_ id: String) throws -> ModelStateResponse {
        guard var record = models[id] else { throw RuntimeError.unknownModel(id) }
        guard record.activeSessions == 0 else { throw RuntimeError.modelBusy(id) }
        guard !record.loading else { throw RuntimeError.modelLoading(id) }
        record.runtime = nil
        record.loading = false
        record.lastUsed = .now
        models[id] = record
        lastActivity = .now
        return stateResponse(record)
    }

    func beginVadSession(_ id: String) async throws -> VadManager {
        _ = try await loadModel(id)
        guard activeSessions < configuration.maxStreamingSessions else {
            throw RuntimeError.sessionLimit(configuration.maxStreamingSessions)
        }
        guard var record = models[id], let runtime = record.runtime else {
            throw RuntimeError.modelLoad(id, "model was not retained after loading")
        }
        guard case .vad(let manager) = runtime else {
            throw RuntimeError.unsupportedStage(id, "vad")
        }
        activeSessions += 1
        record.activeSessions += 1
        record.lastUsed = .now
        models[id] = record
        lastActivity = .now
        return manager
    }

    func endVadSession(_ id: String) {
        endSession(id)
    }

    func beginStreamingSttSession(_ id: String) async throws -> StreamingEouAsrManager {
        _ = try await loadModel(id)
        guard activeSessions < configuration.maxStreamingSessions else {
            throw RuntimeError.sessionLimit(configuration.maxStreamingSessions)
        }
        guard var record = models[id], let runtime = record.runtime else {
            throw RuntimeError.modelLoad(id, "model was not retained after loading")
        }
        guard case .streamingStt(let manager) = runtime else {
            throw RuntimeError.unsupportedStage(id, "streaming_stt")
        }
        guard record.activeSessions == 0 else { throw RuntimeError.modelBusy(id) }
        activeSessions += 1
        record.activeSessions += 1
        record.lastUsed = .now
        models[id] = record
        lastActivity = .now
        await manager.reset()
        return manager
    }

    func endStreamingSttSession(_ id: String) {
        endSession(id)
    }

    func beginDiarizationSession(_ id: String) async throws -> SortformerModels {
        _ = try await loadModel(id)
        guard activeSessions < configuration.maxStreamingSessions else {
            throw RuntimeError.sessionLimit(configuration.maxStreamingSessions)
        }
        guard var record = models[id], let runtime = record.runtime else {
            throw RuntimeError.modelLoad(id, "model was not retained after loading")
        }
        guard case .diarization(let box) = runtime else {
            throw RuntimeError.unsupportedStage(id, "diarization")
        }
        activeSessions += 1
        record.activeSessions += 1
        record.lastUsed = .now
        models[id] = record
        lastActivity = .now
        return box.models
    }

    func endDiarizationSession(_ id: String) {
        endSession(id)
    }

    func transcribeRecording(
        _ id: String,
        audioSamples: [Float],
        language: String?
    ) async throws -> TranscriptionResponse {
        _ = try await loadModel(id)
        guard activeSessions < configuration.maxStreamingSessions else {
            throw RuntimeError.sessionLimit(configuration.maxStreamingSessions)
        }
        guard var record = models[id], let runtime = record.runtime else {
            throw RuntimeError.modelLoad(id, "model was not retained after loading")
        }
        guard case .recordingStt(let manager) = runtime else {
            throw RuntimeError.unsupportedStage(id, "recording_stt")
        }
        activeSessions += 1
        record.activeSessions += 1
        record.lastUsed = .now
        models[id] = record
        lastActivity = .now
        defer { endSession(id) }

        let started = ContinuousClock.now
        let transcript = try await manager.transcribe(
            audioSamples: audioSamples,
            language: language
        )
        let elapsed = started.duration(to: .now)
        let processingMs = max(0, elapsed.components.seconds * 1_000
            + Int64(elapsed.components.attoseconds / 1_000_000_000_000_000))
        let audioDurationMs = UInt64(audioSamples.count) * 1_000 / 16_000
        return TranscriptionResponse(
            transcript: transcript,
            modelId: id,
            model: record.definition.repository,
            variant: manager.variantName,
            language: normalizedLanguage(language),
            audioDurationMs: audioDurationMs,
            processingDurationMs: UInt64(processingMs),
            confidence: nil
        )
    }

    func synthesizeSpeech(
        _ id: String,
        request: SpeechSynthesisRequest
    ) async throws -> SpeechSynthesisResult {
        _ = try await loadModel(id)
        guard activeSessions < configuration.maxStreamingSessions else {
            throw RuntimeError.sessionLimit(configuration.maxStreamingSessions)
        }
        guard var record = models[id], let runtime = record.runtime else {
            throw RuntimeError.modelLoad(id, "model was not retained after loading")
        }
        guard case .tts(let box) = runtime else {
            throw RuntimeError.unsupportedStage(id, "tts")
        }
        guard record.activeSessions == 0 else { throw RuntimeError.modelBusy(id) }

        let text = request.input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw RuntimeError.invalidRequest("speech input is empty") }
        guard text.utf8.count <= 32_000 else {
            throw RuntimeError.invalidRequest("speech input exceeds 32000 bytes")
        }
        let requestedVoice = request.voice?.trimmingCharacters(in: .whitespacesAndNewlines)
        let voice = requestedVoice.flatMap { $0.isEmpty ? nil : $0 }
            ?? record.definition.voice ?? TtsConstants.recommendedVoice
        guard await box.supportsVoice(voice) else {
            throw RuntimeError.invalidRequest("voice \(voice) is not configured for model \(id)")
        }
        let format = request.responseFormat.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard await box.supportsFormat(format), format == "wav" else {
            throw RuntimeError.invalidRequest("format \(format) is not configured for model \(id)")
        }
        let speed = request.speed ?? 1.0
        guard (0.5...2.0).contains(speed) else {
            throw RuntimeError.invalidRequest("speech speed must be between 0.5 and 2.0")
        }
        let localOnly = configuration.offline || configuration.downloadPolicy == .disabled
        if localOnly {
            try Self.requireKokoroAuxiliaryAssets(
                modelID: id,
                cacheDirectory: configuration.modelCacheDir,
                voice: voice,
                requireLexicon: true
            )
        } else {
            try Self.restoreAvailableKokoroAuxiliaryAssets(
                cacheDirectory: configuration.modelCacheDir,
                voice: voice,
                requireLexicon: true
            )
            try Self.requireKokoroAuxiliaryAssets(
                modelID: id,
                cacheDirectory: configuration.modelCacheDir,
                voice: nil,
                requireLexicon: true
            )
        }

        activeSessions += 1
        record.activeSessions += 1
        record.lastUsed = .now
        models[id] = record
        lastActivity = .now
        defer { endSession(id) }

        let started = ContinuousClock.now
        let audio = try await box.synthesize(text: text, voice: voice, speed: speed)
        if !localOnly {
            try Self.persistKokoroAuxiliaryAssets(
                modelID: id,
                cacheDirectory: configuration.modelCacheDir,
                voice: voice,
                requireLexicon: true
            )
            try Self.requireKokoroAuxiliaryAssets(
                modelID: id,
                cacheDirectory: configuration.modelCacheDir,
                voice: voice,
                requireLexicon: true
            )
        }
        guard !audio.isEmpty else { throw RuntimeError.modelLoad(id, "TTS returned empty audio") }
        let elapsed = started.duration(to: .now)
        let processingMs = max(0, elapsed.components.seconds * 1_000
            + Int64(elapsed.components.attoseconds / 1_000_000_000_000_000))
        return SpeechSynthesisResult(
            audio: audio,
            modelId: id,
            model: record.definition.repository,
            voice: voice,
            format: format,
            processingDurationMs: UInt64(processingMs)
        )
    }

    func prewarm() async {
        guard configuration.downloadPolicy == .prewarm else { return }
        for id in configuration.prewarm {
            do { _ = try await loadModel(id) }
            catch { FileHandle.standardError.write(Data("FluidAudio prewarm \(id) failed: \(error)\n".utf8)) }
        }
    }

    func performIdleSweep() -> Bool {
        let now = ContinuousClock.now
        for (id, var record) in models where record.runtime != nil && record.activeSessions == 0 {
            let idle = record.lastUsed.duration(to: now)
            if idle >= .seconds(record.definition.idleSecs) {
                record.runtime = nil
                models[id] = record
            }
        }
        let hasResident = models.values.contains { $0.runtime != nil || $0.loading }
        return activeSessions == 0 && !hasResident
            && lastActivity.duration(to: now) >= .seconds(configuration.processIdleSecs)
    }

    private func stateResponse(_ record: ModelRecord) -> ModelStateResponse {
        ModelStateResponse(
            id: record.definition.id,
            state: record.loading ? "loading" : (record.runtime == nil ? "unloaded" : "loaded"),
            repository: record.definition.repository,
            resident: record.runtime != nil,
            activeSessions: record.activeSessions
        )
    }

    private static func vadModelPath(definition: ModelDefinition, cacheDirectory: String) -> URL {
        URL(fileURLWithPath: cacheDirectory, isDirectory: true)
            .appendingPathComponent("Models", isDirectory: true)
            .appendingPathComponent("silero-vad-coreml", isDirectory: true)
            .appendingPathComponent(
                definition.variant ?? "silero-vad-unified-256ms-v6.0.0.mlmodelc",
                isDirectory: true
            )
    }

    private static func streamingChunkSize(_ variant: String?) throws -> StreamingChunkSize {
        switch variant ?? "160ms" {
        case "160ms": return .ms160
        case "320ms": return .ms320
        default: throw RuntimeError.unsupportedAdapter("unsupported Parakeet EOU variant")
        }
    }

    private static func streamingSttModelPath(
        definition: ModelDefinition,
        cacheDirectory: String
    ) -> URL {
        URL(fileURLWithPath: cacheDirectory, isDirectory: true)
            .appendingPathComponent("parakeet-eou-streaming", isDirectory: true)
            .appendingPathComponent(definition.variant ?? "160ms", isDirectory: true)
    }

    private static func streamingSttModelsExist(at directory: URL) -> Bool {
        [
            "streaming_encoder.mlmodelc",
            "decoder.mlmodelc",
            "joint_decision.mlmodelc",
            "vocab.json",
        ].allSatisfy { FileManager.default.fileExists(atPath: directory.appendingPathComponent($0).path) }
    }

    private static func diarizationModelPath(cacheDirectory: String) -> URL {
        URL(fileURLWithPath: cacheDirectory, isDirectory: true)
            .appendingPathComponent("sortformer", isDirectory: true)
            .appendingPathComponent("SortformerV2.mlmodelc", isDirectory: true)
    }

    private static func kokoroVariant(_ variant: String?) throws -> ModelNames.TTS.Variant {
        switch variant ?? "15s" {
        case "5s": return .fiveSecond
        case "15s": return .fifteenSecond
        default: throw RuntimeError.unsupportedAdapter("unsupported Kokoro TTS variant")
        }
    }

    private static func kokoroModelPath(
        cacheDirectory: String,
        variant: ModelNames.TTS.Variant
    ) -> URL {
        URL(fileURLWithPath: cacheDirectory, isDirectory: true)
            .appendingPathComponent("Models", isDirectory: true)
            .appendingPathComponent("kokoro", isDirectory: true)
            .appendingPathComponent(variant.fileName, isDirectory: true)
    }

    private static func requireKokoroAuxiliaryAssets(
        modelID: String,
        cacheDirectory: String,
        voice: String?,
        requireLexicon: Bool
    ) throws {
        let missing = try copyKokoroAuxiliaryAssets(
            from: kokoroPortableDirectory(cacheDirectory),
            to: try kokoroSdkDirectory(),
            voice: voice,
            requireLexicon: requireLexicon
        )
        if !missing.isEmpty {
            throw RuntimeError.auxiliaryCacheRequired(modelID, missing.joined(separator: ", "))
        }
    }

    private static func restoreAvailableKokoroAuxiliaryAssets(
        cacheDirectory: String,
        voice: String?,
        requireLexicon: Bool
    ) throws {
        _ = try copyKokoroAuxiliaryAssets(
            from: kokoroPortableDirectory(cacheDirectory),
            to: try kokoroSdkDirectory(),
            voice: voice,
            requireLexicon: requireLexicon
        )
    }

    private static func persistKokoroAuxiliaryAssets(
        modelID: String,
        cacheDirectory: String,
        voice: String?,
        requireLexicon: Bool
    ) throws {
        let missing = try copyKokoroAuxiliaryAssets(
            from: try kokoroSdkDirectory(),
            to: kokoroPortableDirectory(cacheDirectory),
            voice: voice,
            requireLexicon: requireLexicon
        )
        if !missing.isEmpty {
            throw RuntimeError.auxiliaryCacheRequired(modelID, missing.joined(separator: ", "))
        }
    }

    private static func kokoroSdkDirectory() throws -> URL {
        try TtsModels.cacheDirectoryURL()
            .appendingPathComponent("Models", isDirectory: true)
            .appendingPathComponent("kokoro", isDirectory: true)
    }

    private static func kokoroPortableDirectory(_ cacheDirectory: String) -> URL {
        URL(fileURLWithPath: cacheDirectory, isDirectory: true)
            .appendingPathComponent("Models", isDirectory: true)
            .appendingPathComponent("kokoro", isDirectory: true)
    }

    @discardableResult
    static func copyKokoroAuxiliaryAssets(
        from sourceDirectory: URL,
        to destinationDirectory: URL,
        voice: String?,
        requireLexicon: Bool
    ) throws -> [String] {
        var missing = [String]()
        for asset in kokoroAuxiliaryAssets(voice: voice, requireLexicon: requireLexicon) {
            let sourceAsset = sourceDirectory.appendingPathComponent(asset.relativePath)
            let destinationAsset = destinationDirectory.appendingPathComponent(asset.relativePath)
            if !FileManager.default.fileExists(atPath: destinationAsset.path),
               FileManager.default.fileExists(atPath: sourceAsset.path) {
                try FileManager.default.createDirectory(
                    at: destinationAsset.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )
                try FileManager.default.copyItem(at: sourceAsset, to: destinationAsset)
            }
            if !FileManager.default.fileExists(atPath: destinationAsset.path) {
                missing.append(asset.label)
            }
        }
        return missing
    }

    static func kokoroAuxiliaryAssets(
        voice: String?,
        requireLexicon: Bool
    ) -> [(relativePath: String, label: String)] {
        var assets = [
            ("vocab_index.json", "Kokoro vocabulary"),
            ("g2p_vocab.json", "G2P vocabulary"),
            ("G2PEncoder.mlmodelc", "G2P encoder"),
            ("G2PDecoder.mlmodelc", "G2P decoder"),
        ]
        if let voice {
            assets.insert(("voices/\(voice).json", "voice \(voice)"), at: 0)
        }
        if requireLexicon {
            assets.append(("us_lexicon_cache.json", "US English lexicon"))
        }
        return assets
    }

    private func enforceCachePolicy(_ definition: ModelDefinition) throws {
        let cached: Bool
        switch definition.adapter {
        case "fluid_audio_vad":
            cached = FileManager.default.fileExists(
                atPath: Self.vadModelPath(
                    definition: definition,
                    cacheDirectory: configuration.modelCacheDir
                ).appendingPathComponent("coremldata.bin").path
            )
        case RecordingSttModelFactory.adapter:
            cached = RecordingSttModelFactory.modelsExist(
                definition: definition,
                cacheRoot: URL(
                    fileURLWithPath: configuration.modelCacheDir,
                    isDirectory: true
                )
            )
        case "fluid_audio_streaming_eou_stt":
            cached = Self.streamingSttModelsExist(at: Self.streamingSttModelPath(
                definition: definition,
                cacheDirectory: configuration.modelCacheDir
            ))
        case "fluid_audio_streaming_sortformer":
            cached = FileManager.default.fileExists(
                atPath: Self.diarizationModelPath(
                    cacheDirectory: configuration.modelCacheDir
                ).path
            )
        case "fluid_audio_kokoro_tts":
            let variant = try Self.kokoroVariant(definition.variant)
            cached = FileManager.default.fileExists(
                atPath: Self.kokoroModelPath(
                    cacheDirectory: configuration.modelCacheDir,
                    variant: variant
                ).path
            )
        default:
            throw RuntimeError.unsupportedAdapter(definition.adapter)
        }
        if !cached && (configuration.offline || configuration.downloadPolicy == .disabled) {
            throw RuntimeError.cacheRequired(definition.id)
        }
    }

    private func verifyChecksumIfConfigured(_ definition: ModelDefinition) throws {
        guard let expected = definition.sha256?.lowercased() else { return }
        guard definition.adapter == "fluid_audio_vad" else {
            throw RuntimeError.unsupportedChecksum(definition.id)
        }
        let url = Self.vadModelPath(
            definition: definition,
            cacheDirectory: configuration.modelCacheDir
        ).appendingPathComponent("coremldata.bin")
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let data = try handle.read(upToCount: 1024 * 1024), !data.isEmpty { hasher.update(data: data) }
        let actual = hasher.finalize().map { String(format: "%02x", $0) }.joined()
        guard actual == expected else { throw RuntimeError.checksumMismatch(definition.id) }
    }

    private func unloadLeastRecentlyUsedIfNeeded(excluding id: String) throws {
        let occupied = models.filter { $0.value.runtime != nil || $0.value.loading }
        guard occupied.count >= configuration.maxResidentModels else { return }
        guard let candidate = occupied
            .filter({ $0.key != id && $0.value.activeSessions == 0 })
            .filter({ !$0.value.loading })
            .min(by: { $0.value.lastUsed < $1.value.lastUsed })
        else { throw RuntimeError.residencyLimit(configuration.maxResidentModels) }
        var record = candidate.value
        record.runtime = nil
        models[candidate.key] = record
    }

    private func endSession(_ id: String) {
        activeSessions = max(0, activeSessions - 1)
        if var record = models[id] {
            record.activeSessions = max(0, record.activeSessions - 1)
            record.lastUsed = .now
            models[id] = record
        }
        lastActivity = .now
    }

    private func normalizedLanguage(_ language: String?) -> String? {
        let code = language?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .split(separator: "-", maxSplits: 1)
            .first
            .map(String.init)
        return code == "auto" || code == "default" || code?.isEmpty == true ? nil : code
    }
}

enum RuntimeError: Error, LocalizedError {
    case unknownModel(String), modelBusy(String), modelLoading(String), modelLoad(String, String)
    case cacheRequired(String), auxiliaryCacheRequired(String, String)
    case checksumMismatch(String), sessionLimit(Int), residencyLimit(Int)
    case unsupportedAdapter(String), unsupportedStage(String, String), unsupportedChecksum(String)
    case invalidRequest(String)
    var errorDescription: String? {
        switch self {
        case .unknownModel(let id): "unknown configured model: \(id)"
        case .modelBusy(let id): "model has active sessions: \(id)"
        case .modelLoading(let id): "model is still loading: \(id)"
        case .modelLoad(let id, let reason): "failed to load \(id): \(reason)"
        case .cacheRequired(let id): "model \(id) is not cached and downloads are disabled"
        case .auxiliaryCacheRequired(let id, let assets):
            "model \(id) is missing required cached Kokoro assets (\(assets))"
        case .checksumMismatch(let id): "model checksum mismatch: \(id)"
        case .sessionLimit(let limit): "streaming session limit reached (\(limit))"
        case .residencyLimit(let limit): "resident model limit reached (\(limit))"
        case .unsupportedAdapter(let adapter): "unsupported FluidAudio adapter: \(adapter)"
        case .unsupportedStage(let id, let stage): "model \(id) does not support \(stage)"
        case .unsupportedChecksum(let id): "model \(id) does not support a single-file checksum"
        case .invalidRequest(let reason): reason
        }
    }
}
