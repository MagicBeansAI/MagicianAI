import Foundation
import MagicianMacAudioEngineCore

enum DownloadPolicy: String, Codable, Sendable {
    case disabled
    case onDemand = "on_demand"
    case prewarm
}

struct ModelDefinition: Codable, Sendable {
    let id: String
    let adapter: String
    let repository: String
    let variant: String?
    let revision: String?
    let sha256: String?
    let idleSecs: UInt64
    let voice: String?
    let voices: [String]?
    let formats: [String]?

    enum CodingKeys: String, CodingKey {
        case id, adapter, repository, variant, revision, sha256, voice, voices, formats
        case idleSecs = "idle_secs"
    }

    init(
        id: String,
        adapter: String,
        repository: String,
        variant: String? = nil,
        revision: String? = nil,
        sha256: String? = nil,
        idleSecs: UInt64,
        voice: String? = nil,
        voices: [String]? = nil,
        formats: [String]? = nil
    ) {
        self.id = id
        self.adapter = adapter
        self.repository = repository
        self.variant = variant
        self.revision = revision
        self.sha256 = sha256
        self.idleSecs = idleSecs
        self.voice = voice
        self.voices = voices
        self.formats = formats
    }
}

struct EngineConfiguration: Codable, Sendable {
    let protocolVersion: Int
    let modelCacheDir: String
    let downloadPolicy: DownloadPolicy
    let registryUrl: String?
    let offline: Bool
    let processIdleSecs: UInt64
    let maxResidentModels: Int
    let maxStreamingSessions: Int
    let maxRequestBytes: Int
    let maxFrameBytes: Int
    let prewarm: [String]
    let models: [ModelDefinition]

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case modelCacheDir = "model_cache_dir"
        case downloadPolicy = "download_policy"
        case registryUrl = "registry_url"
        case offline
        case processIdleSecs = "process_idle_secs"
        case maxResidentModels = "max_resident_models"
        case maxStreamingSessions = "max_streaming_sessions"
        case maxRequestBytes = "max_request_bytes"
        case maxFrameBytes = "max_frame_bytes"
        case prewarm, models
    }

    static func load(environment: [String: String] = ProcessInfo.processInfo.environment) throws
        -> (config: EngineConfiguration, token: String, port: Int)
    {
        guard let token = environment["MAGICIAN_AUDIO_ENGINE_TOKEN"], token.count >= 32 else {
            throw ConfigurationError.invalid("MAGICIAN_AUDIO_ENGINE_TOKEN must contain at least 32 characters")
        }
        guard let json = environment["MAGICIAN_AUDIO_ENGINE_CONFIG_JSON"],
            let data = json.data(using: .utf8)
        else {
            throw ConfigurationError.invalid("MAGICIAN_AUDIO_ENGINE_CONFIG_JSON is required")
        }
        let config = try JSONDecoder().decode(EngineConfiguration.self, from: data)
        guard config.protocolVersion == audioEngineProtocolVersion else {
            throw ConfigurationError.invalid("unsupported protocol version \(config.protocolVersion)")
        }
        guard config.maxResidentModels > 0, config.maxStreamingSessions > 0,
            config.maxRequestBytes > 0, config.maxFrameBytes > 0
        else {
            throw ConfigurationError.invalid("resource limits must be positive")
        }
        guard !config.models.isEmpty else {
            throw ConfigurationError.invalid("at least one configured model is required")
        }
        try config.validateModels()

        let endpoint = environment["MAGICIAN_AUDIO_ENGINE_ENDPOINT"] ?? "http://127.0.0.1:3029"
        guard let components = URLComponents(string: endpoint),
            components.scheme == "http",
            let host = components.host,
            ["127.0.0.1", "localhost", "::1"].contains(host),
            let port = components.port,
            (1...65_535).contains(port),
            components.user == nil,
            components.password == nil,
            components.path.isEmpty || components.path == "/",
            components.query == nil,
            components.fragment == nil
        else {
            throw ConfigurationError.invalid("sidecar endpoint must be an explicit loopback HTTP port")
        }
        return (config, token, port)
    }

    private func validateModels() throws {
        var ids = Set<String>()
        for model in models {
            guard !model.id.isEmpty, ids.insert(model.id).inserted else {
                throw ConfigurationError.invalid("model IDs must be non-empty and unique")
            }
            switch model.adapter {
            case "fluid_audio_vad":
                guard model.repository == "FluidInference/silero-vad-coreml" else {
                    throw ConfigurationError.invalid("unrecognized VAD model repository")
                }
                guard model.variant == nil
                    || model.variant == "silero-vad-unified-256ms-v6.0.0.mlmodelc"
                else {
                    throw ConfigurationError.invalid("unrecognized VAD model variant")
                }
                guard model.revision == nil || model.revision == "main" else {
                    throw ConfigurationError.invalid("FluidAudio 0.12.4 downloads VAD models from revision main")
                }
            case RecordingSttModelFactory.adapter:
                guard model.repository == RecordingSttModelFactory.repository else {
                    throw ConfigurationError.invalid("unrecognized recording STT model repository")
                }
                guard model.variant == nil || ["f32", "int8"].contains(model.variant!) else {
                    throw ConfigurationError.invalid("Qwen3 ASR variant must be f32 or int8")
                }
                guard model.revision == nil || model.revision == "main" else {
                    throw ConfigurationError.invalid("FluidAudio 0.12.4 downloads Qwen3 ASR from revision main")
                }
                guard model.sha256 == nil else {
                    throw ConfigurationError.invalid("Qwen3 ASR uses multiple artifacts and does not accept a single sha256")
                }
            case "fluid_audio_streaming_eou_stt":
                guard model.repository == "FluidInference/parakeet-realtime-eou-120m-coreml" else {
                    throw ConfigurationError.invalid("unrecognized Parakeet EOU model repository")
                }
                guard model.variant == nil || ["160ms", "320ms"].contains(model.variant!) else {
                    throw ConfigurationError.invalid("Parakeet EOU variant must be 160ms or 320ms")
                }
                guard model.revision == nil || model.revision == "main" else {
                    throw ConfigurationError.invalid("FluidAudio 0.12.4 downloads Parakeet EOU from revision main")
                }
                guard model.sha256 == nil else {
                    throw ConfigurationError.invalid("Parakeet EOU uses multiple artifacts and does not accept a single sha256")
                }
            case "fluid_audio_streaming_sortformer":
                guard model.repository == "FluidInference/diar-streaming-sortformer-coreml" else {
                    throw ConfigurationError.invalid("unrecognized Sortformer model repository")
                }
                guard model.variant == nil || model.variant == "default" else {
                    throw ConfigurationError.invalid("Sortformer variant must be default")
                }
                guard model.revision == nil || model.revision == "main" else {
                    throw ConfigurationError.invalid("FluidAudio 0.12.4 downloads Sortformer from revision main")
                }
                guard model.sha256 == nil else {
                    throw ConfigurationError.invalid("Sortformer uses a model bundle and does not accept a single sha256")
                }
            case "fluid_audio_kokoro_tts":
                guard model.repository == "FluidInference/kokoro-82m-coreml" else {
                    throw ConfigurationError.invalid("unrecognized Kokoro TTS model repository")
                }
                guard model.variant == nil || ["5s", "15s"].contains(model.variant!) else {
                    throw ConfigurationError.invalid("Kokoro TTS variant must be 5s or 15s")
                }
                guard model.revision == nil || model.revision == "main" else {
                    throw ConfigurationError.invalid("FluidAudio 0.12.4 downloads Kokoro from revision main")
                }
                guard model.sha256 == nil else {
                    throw ConfigurationError.invalid("Kokoro uses model and voice bundles and does not accept a single sha256")
                }
                guard let voice = model.voice?.trimmingCharacters(in: .whitespaces), !voice.isEmpty else {
                    throw ConfigurationError.invalid("Kokoro TTS requires a configured default voice")
                }
                guard model.voices?.contains(voice) == true else {
                    throw ConfigurationError.invalid("Kokoro TTS default voice must be in its configured voices")
                }
                guard model.formats == ["wav"] else {
                    throw ConfigurationError.invalid("Kokoro TTS currently supports only wav output")
                }
            default:
                throw ConfigurationError.invalid("unsupported FluidAudio adapter \(model.adapter)")
            }
            if let sha256 = model.sha256 {
                guard sha256.count == 64, sha256.allSatisfy({ $0.isHexDigit }) else {
                    throw ConfigurationError.invalid("model sha256 must contain 64 hexadecimal characters")
                }
            }
        }
        let knownIds = Set(models.map(\.id))
        guard prewarm.allSatisfy(knownIds.contains) else {
            throw ConfigurationError.invalid("prewarm references an unknown model ID")
        }
    }
}

enum ConfigurationError: Error, LocalizedError {
    case invalid(String)

    var errorDescription: String? {
        switch self { case .invalid(let reason): reason }
    }
}
