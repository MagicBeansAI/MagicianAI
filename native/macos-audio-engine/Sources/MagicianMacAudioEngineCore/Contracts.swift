import Foundation

public let audioEngineProtocolVersion = 1

public enum AudioSampleFormat: String, CaseIterable, Codable, Sendable {
    case pcmS16Le = "pcm_s16_le"
    case pcmF32Le = "pcm_f32_le"
}

public struct StreamAudioFormat: Codable, Sendable {
    public let sampleRateHz: Int
    public let channels: Int
    public let sampleFormat: AudioSampleFormat

    enum CodingKeys: String, CodingKey {
        case sampleRateHz = "sample_rate_hz"
        case channels
        case sampleFormat = "sample_format"
    }

    public init(sampleRateHz: Int, channels: Int, sampleFormat: AudioSampleFormat) {
        self.sampleRateHz = sampleRateHz
        self.channels = channels
        self.sampleFormat = sampleFormat
    }
}

public struct VadSessionConfiguration: Codable, Sendable {
    public let threshold: Float
    public let minSpeechMs: UInt64
    public let minSilenceMs: UInt64
    public let preRollMs: UInt64
    public let hangoverMs: UInt64
    public let maxUtteranceMs: UInt64
    public let gateOnly: Bool

    enum CodingKeys: String, CodingKey {
        case threshold
        case minSpeechMs = "min_speech_ms"
        case minSilenceMs = "min_silence_ms"
        case preRollMs = "pre_roll_ms"
        case hangoverMs = "hangover_ms"
        case maxUtteranceMs = "max_utterance_ms"
        case gateOnly = "gate_only"
    }
}

public struct StreamingSttSessionConfiguration: Codable, Sendable {
    public let language: String?
    public let eouDebounceMs: UInt64

    enum CodingKeys: String, CodingKey {
        case language
        case eouDebounceMs = "eou_debounce_ms"
    }

    public init(language: String? = nil, eouDebounceMs: UInt64 = 1_280) {
        self.language = language
        self.eouDebounceMs = eouDebounceMs
    }
}

public struct DiarizationSessionConfiguration: Codable, Sendable {
    public let expectedSpeakers: Int?

    enum CodingKeys: String, CodingKey {
        case expectedSpeakers = "expected_speakers"
    }

    public init(expectedSpeakers: Int? = nil) {
        self.expectedSpeakers = expectedSpeakers
    }
}

/// A stage-neutral wire configuration. Each processor validates and consumes
/// only the fields owned by its stage, which keeps one versioned start envelope
/// without making VAD fields mandatory for STT and diarization streams.
public struct StreamStageConfiguration: Codable, Sendable {
    public let threshold: Float?
    public let minSpeechMs: UInt64?
    public let minSilenceMs: UInt64?
    public let preRollMs: UInt64?
    public let hangoverMs: UInt64?
    public let maxUtteranceMs: UInt64?
    public let gateOnly: Bool?
    public let language: String?
    public let eouDebounceMs: UInt64?
    public let expectedSpeakers: Int?

    enum CodingKeys: String, CodingKey {
        case threshold
        case minSpeechMs = "min_speech_ms"
        case minSilenceMs = "min_silence_ms"
        case preRollMs = "pre_roll_ms"
        case hangoverMs = "hangover_ms"
        case maxUtteranceMs = "max_utterance_ms"
        case gateOnly = "gate_only"
        case language
        case eouDebounceMs = "eou_debounce_ms"
        case expectedSpeakers = "expected_speakers"
    }

    public func vad() throws -> VadSessionConfiguration {
        guard let threshold, let minSpeechMs, let minSilenceMs, let preRollMs,
            let hangoverMs, let maxUtteranceMs, let gateOnly
        else {
            throw StreamConfigurationError.invalid("VAD start config is incomplete")
        }
        return VadSessionConfiguration(
            threshold: threshold,
            minSpeechMs: minSpeechMs,
            minSilenceMs: minSilenceMs,
            preRollMs: preRollMs,
            hangoverMs: hangoverMs,
            maxUtteranceMs: maxUtteranceMs,
            gateOnly: gateOnly
        )
    }

    public func streamingStt() -> StreamingSttSessionConfiguration {
        StreamingSttSessionConfiguration(
            language: language,
            eouDebounceMs: eouDebounceMs ?? 1_280
        )
    }

    public func diarization() -> DiarizationSessionConfiguration {
        DiarizationSessionConfiguration(expectedSpeakers: expectedSpeakers)
    }
}

public enum StreamConfigurationError: Error, LocalizedError {
    case invalid(String)

    public var errorDescription: String? {
        switch self { case .invalid(let reason): reason }
    }
}

public struct StreamStartControl: Codable, Sendable {
    public let type: String
    public let protocolVersion: Int
    public let stage: String
    public let modelId: String
    public let format: StreamAudioFormat
    public let config: StreamStageConfiguration

    enum CodingKeys: String, CodingKey {
        case type
        case protocolVersion = "protocol_version"
        case stage
        case modelId = "model_id"
        case format
        case config
    }
}

public struct SpeechSynthesisRequest: Codable, Sendable {
    public let input: String
    public let voice: String?
    public let responseFormat: String
    public let speed: Float?

    enum CodingKeys: String, CodingKey {
        case input, voice, speed
        case responseFormat = "response_format"
    }

    public init(
        input: String,
        voice: String? = nil,
        responseFormat: String = "wav",
        speed: Float? = nil
    ) {
        self.input = input
        self.voice = voice
        self.responseFormat = responseFormat
        self.speed = speed
    }
}

public enum AudioEngineStreamEvent: Equatable, Sendable {
    case ready
    case probability(value: Float, atMs: UInt64)
    case speechStarted(atMs: UInt64)
    case speechEnded(atMs: UInt64)
    case transcriptPartial(text: String, turnId: String, startMs: UInt64)
    case transcriptFinal(text: String, turnId: String, language: String?, startMs: UInt64)
    case speakerStarted(speakerId: String, atMs: UInt64)
    case speakerEnded(speakerId: String, atMs: UInt64)
    case segmentRevised(
        speakerId: String,
        startMs: UInt64,
        endMs: UInt64,
        confidence: Float?
    )
    case finished
    case error(code: String, message: String)
}

extension AudioEngineStreamEvent: Encodable {
    enum CodingKeys: String, CodingKey {
        case type, value, text, language, confidence, code, message
        case atMs = "at_ms"
        case turnId = "turn_id"
        case startMs = "start_ms"
        case endMs = "end_ms"
        case speakerId = "speaker_id"
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .ready:
            try container.encode("ready", forKey: .type)
        case .probability(let value, let atMs):
            try container.encode("probability", forKey: .type)
            try container.encode(value, forKey: .value)
            try container.encode(atMs, forKey: .atMs)
        case .speechStarted(let atMs):
            try container.encode("speech_started", forKey: .type)
            try container.encode(atMs, forKey: .atMs)
        case .speechEnded(let atMs):
            try container.encode("speech_ended", forKey: .type)
            try container.encode(atMs, forKey: .atMs)
        case .transcriptPartial(let text, let turnId, let startMs):
            try container.encode("transcript_partial", forKey: .type)
            try container.encode(text, forKey: .text)
            try container.encode(turnId, forKey: .turnId)
            try container.encode(startMs, forKey: .startMs)
        case .transcriptFinal(let text, let turnId, let language, let startMs):
            try container.encode("transcript_final", forKey: .type)
            try container.encode(text, forKey: .text)
            try container.encode(turnId, forKey: .turnId)
            try container.encodeIfPresent(language, forKey: .language)
            try container.encode(startMs, forKey: .startMs)
        case .speakerStarted(let speakerId, let atMs):
            try container.encode("speaker_started", forKey: .type)
            try container.encode(speakerId, forKey: .speakerId)
            try container.encode(atMs, forKey: .atMs)
        case .speakerEnded(let speakerId, let atMs):
            try container.encode("speaker_ended", forKey: .type)
            try container.encode(speakerId, forKey: .speakerId)
            try container.encode(atMs, forKey: .atMs)
        case .segmentRevised(let speakerId, let startMs, let endMs, let confidence):
            try container.encode("segment_revised", forKey: .type)
            try container.encode(speakerId, forKey: .speakerId)
            try container.encode(startMs, forKey: .startMs)
            try container.encode(endMs, forKey: .endMs)
            try container.encodeIfPresent(confidence, forKey: .confidence)
        case .finished:
            try container.encode("finished", forKey: .type)
        case .error(let code, let message):
            try container.encode("error", forKey: .type)
            try container.encode(code, forKey: .code)
            try container.encode(message, forKey: .message)
        }
    }
}
