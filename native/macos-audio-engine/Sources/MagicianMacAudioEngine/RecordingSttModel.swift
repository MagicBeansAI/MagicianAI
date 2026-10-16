import FluidAudio
import Foundation

protocol RecordingSttModel: Sendable {
    var variantName: String { get }
    func transcribe(audioSamples: [Float], language: String?) async throws -> String
}

enum RecordingSttModelFactory {
    static let adapter = "fluid_audio_recording_stt"
    static let repository = "FluidInference/qwen3-asr-0.6b-coreml"

    static func load(
        definition: ModelDefinition,
        cacheRoot: URL,
        localOnly: Bool
    ) async throws -> any RecordingSttModel {
        guard definition.repository == repository else {
            throw RecordingSttError.unsupportedRepository(definition.repository)
        }
        guard #available(macOS 15, *) else {
            throw RecordingSttError.unsupportedPlatform
        }
        return try await loadQwen3(
            definition: definition,
            cacheRoot: cacheRoot,
            localOnly: localOnly
        )
    }

    static func modelsExist(definition: ModelDefinition, cacheRoot: URL) -> Bool {
        let variant = definition.variant ?? "f32"
        let directory = cacheRoot
            .appendingPathComponent("qwen3-asr-0.6b-coreml", isDirectory: true)
            .appendingPathComponent(variant, isDirectory: true)
        return [
            "qwen3_asr_audio_encoder.mlmodelc",
            "qwen3_asr_decoder_stateful.mlmodelc",
            "qwen3_asr_embeddings.bin",
            "vocab.json",
        ].allSatisfy { FileManager.default.fileExists(atPath: directory.appendingPathComponent($0).path) }
    }

    @available(macOS 15, *)
    private static func loadQwen3(
        definition: ModelDefinition,
        cacheRoot: URL,
        localOnly: Bool
    ) async throws -> any RecordingSttModel {
        let variant = try qwenVariant(definition.variant)
        let modelDirectory = cacheRoot.appendingPathComponent(
            variant.repo.folderName,
            isDirectory: true
        )
        if !Qwen3AsrModels.modelsExist(at: modelDirectory) {
            guard !localOnly else {
                throw RecordingSttError.cacheRequired(definition.id)
            }
            try FileManager.default.createDirectory(
                at: cacheRoot,
                withIntermediateDirectories: true
            )
            try await DownloadUtils.downloadRepo(variant.repo, to: cacheRoot)
        }
        guard Qwen3AsrModels.modelsExist(at: modelDirectory) else {
            throw RecordingSttError.incompleteModel(definition.id)
        }
        return try await Qwen3RecordingSttModel(
            modelDirectory: modelDirectory,
            variant: variant
        )
    }

    @available(macOS 15, *)
    private static func qwenVariant(_ configured: String?) throws -> Qwen3AsrVariant {
        let value = configured?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            ?? "f32"
        guard let variant = Qwen3AsrVariant(rawValue: value) else {
            throw RecordingSttError.unsupportedVariant(value)
        }
        return variant
    }
}

@available(macOS 15, *)
private actor Qwen3RecordingSttModel: RecordingSttModel {
    nonisolated let variantName: String
    private let manager: Qwen3AsrManager

    init(modelDirectory: URL, variant: Qwen3AsrVariant) async throws {
        self.variantName = variant.rawValue
        self.manager = Qwen3AsrManager()
        try await manager.loadModels(from: modelDirectory)
    }

    func transcribe(audioSamples: [Float], language: String?) async throws -> String {
        guard !audioSamples.isEmpty else { throw RecordingSttError.emptyAudio }
        guard Double(audioSamples.count) / 16_000 <= Qwen3AsrConfig.maxAudioSeconds else {
            throw RecordingSttError.audioTooLong(Qwen3AsrConfig.maxAudioSeconds)
        }
        let normalizedLanguage = try language.flatMap(normalizeLanguage)
        return try await manager.transcribe(
            audioSamples: audioSamples,
            language: normalizedLanguage,
            maxNewTokens: 512
        )
    }

    private func normalizeLanguage(_ value: String) throws -> Qwen3AsrConfig.Language? {
        let code = value
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .split(separator: "-", maxSplits: 1)
            .first
            .map(String.init) ?? ""
        if code.isEmpty || code == "auto" || code == "default" { return nil }
        guard let language = Qwen3AsrConfig.Language(from: code) else {
            throw RecordingSttError.unsupportedLanguage(value)
        }
        return language
    }
}

enum RecordingAudioDecoder {
    static func decode(_ data: Data, contentType: String) async throws -> [Float] {
        guard !data.isEmpty else { throw RecordingSttError.emptyAudio }
        let fileExtension = extensionForContentType(contentType)
        return try await Task.detached(priority: .userInitiated) {
            let directory = FileManager.default.temporaryDirectory
                .appendingPathComponent("magician-audio-engine", isDirectory: true)
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true
            )
            let url = directory
                .appendingPathComponent(UUID().uuidString)
                .appendingPathExtension(fileExtension)
            defer { try? FileManager.default.removeItem(at: url) }
            try data.write(to: url, options: [.atomic])
            return try AudioConverter().resampleAudioFile(url)
        }.value
    }

    private static func extensionForContentType(_ contentType: String) -> String {
        let type = contentType.lowercased().split(separator: ";", maxSplits: 1).first.map(String.init)
        switch type {
        case "audio/wav", "audio/wave", "audio/x-wav": return "wav"
        case "audio/mp4", "audio/m4a", "audio/x-m4a": return "m4a"
        case "audio/aac": return "aac"
        case "audio/mpeg", "audio/mp3": return "mp3"
        case "audio/aiff", "audio/x-aiff": return "aiff"
        default: return "audio"
        }
    }
}

enum RecordingSttError: Error, LocalizedError {
    case emptyAudio
    case audioTooLong(Double)
    case unsupportedLanguage(String)
    case unsupportedPlatform
    case unsupportedRepository(String)
    case unsupportedVariant(String)
    case cacheRequired(String)
    case incompleteModel(String)

    var errorDescription: String? {
        switch self {
        case .emptyAudio: "recording STT requires non-empty audio"
        case .audioTooLong(let seconds): "recording exceeds the model's \(Int(seconds)) second limit"
        case .unsupportedLanguage(let language): "unsupported Qwen3 ASR language: \(language)"
        case .unsupportedPlatform: "Qwen3 ASR requires macOS 15 or newer"
        case .unsupportedRepository(let repository): "unsupported recording STT repository: \(repository)"
        case .unsupportedVariant(let variant): "unsupported Qwen3 ASR variant: \(variant)"
        case .cacheRequired(let id): "model \(id) is not cached and downloads are disabled"
        case .incompleteModel(let id): "model \(id) did not download all required artifacts"
        }
    }
}
